#![no_std]
#![no_main]

extern crate alloc;

mod debug;
mod sd;
#[allow(dead_code)] // some control-handler accessors unused until composite CDC
mod uac_source;
mod usb_audio;
mod usb_cdc;

use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, Ordering};

use daisy_embassy::audio::{AudioPeripherals, HALF_DMA_BUFFER_LENGTH};
use daisy_embassy::led::UserLed;
use daisy_embassy::{hal, new_daisy_board};
use dsp::freeze::{self, Freeze, GlitchTape};
use dsp::limiter::Limiter;
use dsp::tape::TapeProcessor;
use defmt::info;
use embassy_executor::{InterruptExecutor, Spawner};
use embassy_futures::join::join;
use embassy_futures::yield_now;
use embassy_stm32::interrupt;
use embassy_stm32::interrupt::{InterruptExt, Priority};
use embassy_time::{Delay, Timer};
use embedded_alloc::LlffHeap as Heap;
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use embedded_sdmmc::{Mode, VolumeIdx, VolumeManager};
use heapless::spsc::{Consumer, Producer, Queue};
use static_cell::StaticCell;

// defmt-rtt stays the defmt global logger (info! -> RTT, unread without a
// probe). Panic handler + readable logs are in `debug` (UART on D2).
use defmt_rtt as _;

#[global_allocator]
static HEAP: Heap = Heap::empty();
// Heap in on-chip AXI SRAM (cached, fast) — NOT external SDRAM. The DSP delay
// lines (tape ~22 KB + freeze ring ~115 KB) thrash the 16 KB D-cache, so they
// must sit in fast on-chip memory (an AXI miss is ~12x cheaper than an SDRAM
// miss). 256 KB fits the FX with headroom; `.axisram_bss` maps to AXI SRAM.
const HEAP_SIZE: usize = 256 * 1024;
#[unsafe(link_section = ".axisram_bss")]
static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

const SAMPLE_RATE: f32 = 48_000.0;

const RING_LEN: usize = 8192;
static RING: StaticCell<Queue<i16, RING_LEN>> = StaticCell::new();

// Second SPSC ring: the SAI callback tees the played samples here and the USB
// stream task drains them into the iso-IN endpoint. Kept small so it stays
// near-empty in steady state (low capture latency); overflow is dropped while
// the host isn't capturing.
const USB_RING_LEN: usize = 512;
static USB_RING: StaticCell<Queue<i16, USB_RING_LEN>> = StaticCell::new();

// Stereo frames the SAI callback has actually played. The CDC position task
// derives song position from this (per audio sample rendered, not wall-clock,
// so it can't drift from the audio — see PLAN_USB_COMPOSITE complication #3).
static PLAYED_FRAMES: AtomicU32 = AtomicU32::new(0);

// Audio health counters, logged once/second by the heartbeat over the debug
// UART. cb_full catches any callback stall/preemption; sai_err counts SAI
// underruns (the glitch event); sd_under counts an empty SD ring; peak is the
// post-FX output level (×1000).
static OUT_PEAK_MILLI: AtomicU32 = AtomicU32::new(0);
static CB_FULL_US: AtomicU32 = AtomicU32::new(0);
static SAI_ERR: AtomicU32 = AtomicU32::new(0);
static SD_UNDERRUN: AtomicU32 = AtomicU32::new(0);

// 2b: audio runs on a high-priority interrupt executor so blocking SD reads
// (and, later, DSP/MIDI) on the thread executor can never starve the SAI
// refill. UART4's vector is unused by the app and just drives this executor —
// any otherwise-free interrupt vector works.
static AUDIO_EXEC: InterruptExecutor = InterruptExecutor::new();

#[interrupt]
unsafe fn UART4() {
    unsafe { AUDIO_EXEC.on_interrupt() }
}

// LED (thread executor): 1 flash = no card, 2 = FAT mount, 3 = AMBIENT.RAW
// open failed; steady 1 Hz = streaming. Panics print over the debug UART.

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Enable FPU flush-to-zero + default-NaN. The DSP filter tails decay into
    // denormals; without FZ the Cortex-M7 traps them to glacial support code
    // (~300 ms/block measured -> silence via SAI underrun). FPDSCR sets the
    // default for exception contexts — the audio FX runs in the UART4 interrupt
    // executor — and FPSCR sets the current (thread) context.
    unsafe {
        const FPDSCR: *mut u32 = 0xE000_EF3C as *mut u32;
        core::ptr::write_volatile(FPDSCR, (1 << 24) | (1 << 25)); // FZ | DN
        let mut fpscr: u32;
        core::arch::asm!("vmrs {}, fpscr", out(reg) fpscr);
        fpscr |= (1 << 24) | (1 << 25);
        core::arch::asm!("vmsr fpscr, {}", in(reg) fpscr);
    }

    let p = hal::init(daisy_embassy::default_rcc());
    info!("ambient-viz-daisy firmware: SD stream + DSP (SDRAM heap)");

    let board = new_daisy_board!(p);
    let mut led = board.user_led;

    // Debug UART on USART3 TX (D2 / PC10), 115200, read on the Shikra.
    let mut dbg_cfg = embassy_stm32::usart::Config::default();
    dbg_cfg.baudrate = 115_200;
    let dbg_tx =
        embassy_stm32::usart::UartTx::new_blocking(p.USART3, board.pins.d2, dbg_cfg).unwrap();
    debug::init(dbg_tx);
    dbg_uart!("=== ambient-viz-daisy boot ===");

    // Bring up external SDRAM and relocate the global heap into it — the DSP FX
    // buffers don't fit the 64 KB internal RAM. Must precede any allocation.
    let mut cm = cortex_m::Peripherals::take().unwrap();
    let mut sdram = board.sdram.build(&mut cm.MPU, &mut cm.SCB);
    let mut sdram_delay = Delay;
    let sdram_addr = sdram.init(&mut sdram_delay) as usize; // SDRAM up (for the future Engine)
    unsafe { HEAP.init((&raw mut HEAP_MEM) as usize, HEAP_SIZE) };
    dbg_uart!(
        "heap: {} KB in AXI SRAM; SDRAM 64M ready @ {:#010x}",
        HEAP_SIZE / 1024,
        sdram_addr
    );

    // Caches: the DSP is data-bound on the external SDRAM, so the D-cache is the
    // real fix (I-cache alone bought ~14%). The SDRAM maps at 0xC000_0000 (FMC
    // bank 1), which the default memory map treats as non-cacheable device, so
    // we add our own MPU regions: SDRAM cacheable write-back for fast DSP, and
    // SRAM1 (where the SAI DMA buffers live, `.sram1_bss`) non-cacheable so the
    // DMA stays coherent with the cache. Then enable both caches.
    cm.SCB.enable_icache();
    unsafe {
        use cortex_m::asm::{dmb, dsb, isb};
        dmb();
        cm.MPU.ctrl.write(0); // disable while editing regions
        // Region 1: SDRAM @ 0xC000_0000, 8 MB, normal cacheable write-back
        // (AP=full, C=1, B=1, SIZE=2^23).
        cm.MPU.rnr.write(1);
        cm.MPU.rbar.write(0xC000_0000);
        cm.MPU.rasr.write((0b011 << 24) | (1 << 17) | (1 << 16) | ((23 - 1) << 1) | 1);
        // Region 2: SRAM1 @ 0x3000_0000, 128 KB, normal non-cacheable
        // (AP=full, TEX=001/C=0/B=0, SIZE=2^17) — keeps SAI DMA coherent.
        cm.MPU.rnr.write(2);
        cm.MPU.rbar.write(0x3000_0000);
        cm.MPU.rasr.write((0b011 << 24) | (0b001 << 19) | ((17 - 1) << 1) | 1);
        // Re-enable MPU: background default map for privileged + MPU on.
        cm.MPU.ctrl.write(0x05);
        dsb();
        isb();
    }
    cm.SCB.enable_dcache(&mut cm.CPUID);
    dbg_uart!("cache: I+D on (SDRAM cacheable, SRAM1 DMA non-cacheable)");

    // Hand the audio peripherals to the interrupt-executor task (below). Doing
    // all SAI setup inside the task keeps the non-Send Interface from crossing
    // the executor boundary — only the Send AudioPeripherals + Consumer do.
    let audio_peripherals = board.audio_peripherals;

    let sdcard = sd::build_sd_card(
        p.SPI1,
        board.pins.d8,  // PG11 / SCK
        board.pins.d10, // PB5  / MOSI
        board.pins.d9,  // PB4  / MISO
        board.pins.d7,  // PG10 / CS
    );

    // Acquire at the slow init clock, retrying a few times — cold-boot supply +
    // card settling makes the first attempt flaky on the crowded breakout. Then
    // bump to full speed for streaming.
    let mut sd_ok = false;
    for attempt in 1..=5u8 {
        if sdcard.num_bytes().is_ok() {
            sd_ok = true;
            break;
        }
        dbg_uart!("SD: init attempt {} failed, retrying", attempt);
        sdcard.mark_card_uninit();
        Timer::after_millis(100).await;
    }
    if !sd_ok {
        dbg_uart!("SD: no card / init failed after 5 tries (blink 1)");
        blink_code(&mut led, 1).await;
    }
    sd::set_fast(&sdcard);
    dbg_uart!("SD: acquired at 400kHz, SPI -> 24MHz for streaming");
    let volume_mgr = VolumeManager::new(sdcard, sd::ZeroTime);
    let volume = match volume_mgr.open_volume(VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => {
            dbg_uart!("SD: FAT volume mount failed (blink 2)");
            blink_code(&mut led, 2).await
        }
    };
    let root = match volume.open_root_dir() {
        Ok(r) => r,
        Err(_) => {
            dbg_uart!("SD: open root dir failed (blink 2)");
            blink_code(&mut led, 2).await
        }
    };
    let file = match root.open_file_in_dir("AMBIENT.RAW", Mode::ReadOnly) {
        Ok(f) => f,
        Err(_) => {
            dbg_uart!("SD: AMBIENT.RAW open failed (blink 3)");
            blink_code(&mut led, 3).await
        }
    };
    dbg_uart!("SD: streaming AMBIENT.RAW, {} bytes", file.length());
    // Loop length in stereo frames (4 bytes/frame) for CDC position wrap.
    let loop_frames = (file.length() / 4) as u64;

    let q = RING.init(Queue::new());
    let (mut producer, consumer) = q.split();
    let usb_q = USB_RING.init(Queue::new());
    let (usb_producer, usb_consumer) = usb_q.split();

    // Spawn the audio consumer on the high-priority interrupt executor.
    interrupt::UART4.set_priority(Priority::P6);
    let audio_spawner = AUDIO_EXEC.start(interrupt::UART4);
    audio_spawner.must_spawn(audio_task(audio_peripherals, consumer, usb_producer));
    dbg_uart!("audio: task spawned (interrupt executor, UART4/P6)");

    // --- USB: composite UAC1 audio source + CDC ACM over OTG-FS -------------
    // 512-byte config descriptor: UAC source + CDC won't fit the default 256.
    static CONFIG_DESC: StaticCell<[u8; 512]> = StaticCell::new();
    static BOS_DESC: StaticCell<[u8; 32]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
    static EP_OUT: StaticCell<[u8; usb_audio::EP_OUT_BUF]> = StaticCell::new();
    static UAC_HANDLER: StaticCell<uac_source::AudioSourceControlHandler> = StaticCell::new();

    let mut usb_cfg = embassy_stm32::usb::Config::default();
    usb_cfg.vbus_detection = false; // safe default; Daisy is bus-powered
    let usb_driver = embassy_stm32::usb::Driver::new_fs(
        board.usb_peripherals.usb_otg_fs,
        usb_audio::Irqs,
        board.usb_peripherals.pins.DP,
        board.usb_peripherals.pins.DN,
        EP_OUT.init([0u8; usb_audio::EP_OUT_BUF]),
        usb_cfg,
    );

    let mut dev_cfg = embassy_usb::Config::new(0x1209, 0x0001); // pid.codes test VID
    dev_cfg.manufacturer = Some("ambient-viz");
    dev_cfg.product = Some("Daisy audio source");
    dev_cfg.serial_number = Some("0001");
    // IAD device class so the (future) composite UAC + CDC enumerates cleanly.
    dev_cfg.device_class = 0xEF;
    dev_cfg.device_sub_class = 0x02;
    dev_cfg.device_protocol = 0x01;
    dev_cfg.composite_with_iads = true;

    let mut usb_builder = embassy_usb::Builder::new(
        usb_driver,
        dev_cfg,
        CONFIG_DESC.init([0; 512]),
        BOS_DESC.init([0; 32]),
        &mut [],
        CONTROL_BUF.init([0; 64]),
    );

    let (uac_audio_ep, _uac_feedback_ep, uac_handler) = uac_source::AudioSource::new(
        &mut usb_builder,
        &usb_audio::SAMPLE_RATES,
        embassy_usb::class::uac1::SampleWidth::Width2Byte,
        usb_audio::FEEDBACK_REFRESH_MS,
    );
    usb_builder.handler(UAC_HANDLER.init(uac_handler));

    // CDC ACM in the same composite (Phase C): song-position out to the Pi.
    // Full-duplex; the inbound (sensor-MIDI) leg is Phase E, pending Engine.
    static CDC_STATE: StaticCell<CdcState> = StaticCell::new();
    let cdc = CdcAcmClass::new(&mut usb_builder, CDC_STATE.init(CdcState::new()), 64);

    let usb_device = usb_builder.build();
    // Keep USB below the audio interrupt executor (P6) so OTG IRQs can't preempt
    // the audio callback mid-write and starve the SAI (glitches). USB tolerates
    // the ~callback-length delay; audio must not.
    interrupt::OTG_FS.set_priority(Priority::P7);
    spawner.must_spawn(usb_audio::usb_task(usb_device));
    spawner.must_spawn(usb_audio::stream_task(uac_audio_ep, usb_consumer));
    spawner.must_spawn(usb_cdc::position_emit_task(cdc, loop_frames));
    dbg_uart!("usb: UAC source + CDC position built + tasks spawned");

    // Producer + heartbeat on the thread executor. Blocking SD reads here can
    // no longer glitch the audio — the interrupt executor preempts them.
    let heartbeat = async {
        loop {
            led.on();
            Timer::after_millis(500).await;
            led.off();
            Timer::after_millis(500).await;
            // Safe now that dbg_uart writes per-byte (≤87 µs CS, not ~6 ms).
            // cb_full spikes ~730 us on loss-FIR rebuild blocks but they're
            // isolated; sai_err (SAI underruns) is the real health metric.
            dbg_uart!(
                "diag: cb_full {} us | sai_err {} sd_under {} | peak {}",
                CB_FULL_US.swap(0, Ordering::Relaxed),
                SAI_ERR.swap(0, Ordering::Relaxed),
                SD_UNDERRUN.swap(0, Ordering::Relaxed),
                OUT_PEAK_MILLI.swap(0, Ordering::Relaxed),
            );
        }
    };
    let reader = async {
        let mut block = [0u8; 512];
        loop {
            if file.is_eof() {
                let _ = file.seek_from_start(0);
            }
            let n = file.read(&mut block).unwrap_or(0);
            if n == 0 {
                let _ = file.seek_from_start(0);
                yield_now().await;
                continue;
            }
            let mut i = 0;
            while i + 1 < n {
                let s = i16::from_le_bytes([block[i], block[i + 1]]);
                while producer.enqueue(s).is_err() {
                    yield_now().await;
                }
                i += 2;
            }
            yield_now().await;
        }
    };
    join(heartbeat, reader).await;
}

/// Audio output, on the interrupt executor. Drains L,R pairs from the ring into
/// each SAI block; silence on underrun. With audio here, SD read duration on
/// the thread executor is irrelevant to refill timing.
#[embassy_executor::task]
async fn audio_task(
    audio: AudioPeripherals<'static>,
    mut consumer: Consumer<'static, i16, RING_LEN>,
    mut usb_producer: Producer<'static, i16, USB_RING_LEN>,
) {
    let interface = audio.prepare_interface(Default::default()).await;
    let mut interface = match interface.start_interface().await {
        Ok(i) => i,
        Err(_) => {
            dbg_uart!("audio: start_interface FAILED");
            return;
        }
    };
    // Master FX chain applied to the SD stream. We run the effects standalone
    // (the synth Engine ignores external input), mirroring its master order:
    // tape -> freeze send (glitch + return while active) -> limiter. Buffers
    // (~155 KB) come from the SDRAM heap.
    let mut tape = TapeProcessor::new(SAMPLE_RATE);
    tape.set_enabled(true);
    let mut freeze = Freeze::new(SAMPLE_RATE);
    let mut glitch = GlitchTape::new(SAMPLE_RATE);
    let mut limiter = Limiter::new(SAMPLE_RATE);
    // Prime tape's scratch (it resizes on first process()) so the RT callback
    // never allocates.
    tape.process(&mut [0.0f32; HALF_DMA_BUFFER_LENGTH], 0);
    let mut sample_index: u64 = 0;
    dbg_uart!("audio: interface started, FX chain ready");

    loop {
        // start_callback returns only on a SAI error; on its own executor that
        // shouldn't happen now. Restart rather than panic if it ever does.
        let _ = interface
            .start_callback(|_input, output| {
                let cb_t = embassy_time::Instant::now();
                let n = output.len().min(HALF_DMA_BUFFER_LENGTH);
                let mut buf = [0.0f32; HALF_DMA_BUFFER_LENGTH];
                let mut send = [0.0f32; HALF_DMA_BUFFER_LENGTH];

                // SD i16 -> f32 master block.
                for s in buf[..n].iter_mut() {
                    let v = match consumer.dequeue() {
                        Some(v) => v,
                        None => {
                            SD_UNDERRUN.fetch_add(1, Ordering::Relaxed);
                            0
                        }
                    };
                    *s = v as f32 / 32768.0;
                }

                // Placeholder control until the CDC CC path (Phase E): sweep tape
                // failure 0..1 over 20 s (now cheap to retune live — the loss FIR
                // rebuild uses a cosine LUT) and freeze a grain every 10 s.
                let t = sample_index as f32 / SAMPLE_RATE;
                let xf = {
                    let x = t * (1.0 / 20.0);
                    x - (x as u32 as f32) // fract(t / 20)
                };
                tape.set_failure(if xf < 0.5 { xf * 2.0 } else { 2.0 - xf * 2.0 });
                let yf = {
                    let y = t * (1.0 / 10.0);
                    y - (y as u32 as f32) // fract(t / 10)
                };
                freeze.set_amount(if yf * 10.0 < 0.5 { 1.0 } else { 0.0 });

                // Master chain (mirrors Engine::process): tape -> freeze send
                // (glitch + return while active) -> limiter.
                tape.process(&mut buf[..n], sample_index);
                freeze.process(&buf[..n], &mut send[..n]);
                if freeze.active() {
                    glitch.process(&mut send[..n]);
                    for (o, &g) in buf[..n].iter_mut().zip(send[..n].iter()) {
                        *o += g * freeze::FREEZE_RETURN_GAIN;
                    }
                }
                limiter.process(&mut buf[..n]);

                let mut pk = 0.0f32;
                for &s in buf[..n].iter() {
                    let a = if s < 0.0 { -s } else { s };
                    if a > pk {
                        pk = a;
                    }
                }
                OUT_PEAK_MILLI.fetch_max((pk * 1000.0) as u32, Ordering::Relaxed);

                // f32 -> SAI 24-bit, and tee the *processed* (heard) audio to USB.
                for (i, frame) in output[..n].chunks_mut(2).enumerate() {
                    let l = buf[2 * i];
                    let r = buf[2 * i + 1];
                    frame[0] = f32_to_u24(l);
                    frame[1] = f32_to_u24(r);
                    let _ = usb_producer.enqueue(f32_to_i16(l));
                    let _ = usb_producer.enqueue(f32_to_i16(r));
                }

                let frames = (n / 2) as u64;
                sample_index += frames;
                PLAYED_FRAMES.fetch_add(frames as u32, Ordering::Relaxed);
                CB_FULL_US.fetch_max(cb_t.elapsed().as_micros() as u32, Ordering::Relaxed);
            })
            .await;
        // start_callback returns Result<Infallible, _>, so a return == a SAI
        // error (underrun/overrun) — the glitch event. Count + restart.
        SAI_ERR.fetch_add(1, Ordering::Relaxed);
    }
}

/// Repeating LED code: `code` quick flashes then a pause. Never returns.
async fn blink_code(led: &mut UserLed<'_>, code: u8) -> ! {
    loop {
        for _ in 0..code {
            led.on();
            Timer::after_millis(150).await;
            led.off();
            Timer::after_millis(150).await;
        }
        Timer::after_millis(800).await;
    }
}

/// f32 [-1,1] -> 24-bit signed in a u32's low bits, the SAI callback's format
/// (sign-extended i32-as-u32, matching the old i16<<8 path the codec expects).
#[inline(always)]
fn f32_to_u24(s: f32) -> u32 {
    ((s.clamp(-1.0, 1.0) * 8_388_607.0) as i32) as u32
}

/// f32 [-1,1] -> i16, for the USB capture tee.
#[inline(always)]
fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32_767.0) as i16
}
