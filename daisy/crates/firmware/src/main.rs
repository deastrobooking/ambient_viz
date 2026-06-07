#![no_std]
#![no_main]

extern crate alloc;

mod debug;
mod sd;
#[cfg(feature = "debug-uart")]
mod temp;
#[allow(dead_code)] // some control-handler accessors unused until composite CDC
mod uac_source;
mod usb_audio;
mod usb_cdc;

use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use daisy_embassy::audio::{AudioPeripherals, HALF_DMA_BUFFER_LENGTH};
use daisy_embassy::led::UserLed;
use daisy_embassy::{hal, new_daisy_board};
use defmt::info;
#[cfg(feature = "voice")]
use dsp::PainMaterialVoice;
#[cfg(feature = "freeze")]
use dsp::freeze::{self, Freeze, GlitchTape};
use dsp::limiter::Limiter;
use dsp::tape::TapeProcessor;
#[cfg(feature = "bell")]
use dsp::{AudioParam, FmPatch, FmStab, FrameProcessor as _, PingPongDelay};
use embassy_executor::{InterruptExecutor, Spawner};
use embassy_futures::join::join;
use embassy_futures::yield_now;
use embassy_stm32::interrupt;
use embassy_stm32::interrupt::{InterruptExt, Priority};
use embassy_time::{Delay, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use embedded_alloc::LlffHeap as Heap;
use embedded_sdmmc::{Mode, VolumeIdx, VolumeManager};
use heapless::spsc::{Consumer, Producer, Queue};
use static_cell::StaticCell;

// defmt-rtt stays the defmt global logger (info! -> RTT, unread without a
// probe). Panic handler + readable logs are in `debug` (UART on D2).
use defmt_rtt as _;

#[global_allocator]
static HEAP: Heap = Heap::empty();
// Heap in on-chip AXI SRAM (cached, fast) — NOT external SDRAM. The DSP delay
// lines must sit in fast on-chip memory (an AXI miss is ~12x cheaper than an
// SDRAM miss), and `.axisram_bss` maps to the 512 KB AXI SRAM, of which the
// heap is the SOLE occupant (stack + .bss/.data live in DTCM via the RAM alias,
// SAI DMA buffers in D2 SRAM, SDRAM separate). NOLOAD .bss → zero flash cost.
//
// Footprint of the full prod FX chain, MEASURED by the host probe
// `cargo run -p host --bin heap_probe` (allocation sizes are identical on host
// and Daisy — f32 = 4 B, same Vec growth; the probe's "198 KB before voice"
// matched the on-device heap log exactly):
//   tape + limiter                              25 KB
//   bell (FmStab + ping-pong ring)             172 KB   ← bigger than it looks
//   voice @ 48 kHz (Stutter + low-mem reverb)  204 KB
//   ---------------------------------------------------
//   bell + voice peak                         ~403 KB   (no transient now)
//
// History: this used to need 44.1 kHz + a 640 KB transient dance, because
// SpeechSynth::new triggered a Stutter `Vec::resize` that DOUBLED the ring while
// holding the old buffer. The vendored infinitedsp-core patch makes that resize
// a no-op, so 48 kHz fits with no spike (peak == steady). 504 KB leaves ~100 KB
// of margin (and room toward `freeze`), using the whole 512 KB AXI region minus
// an 8 KB linker slack. 448 KB would also fit; 504 is just conservative.
const HEAP_SIZE: usize = 504 * 1024;
#[unsafe(link_section = ".axisram_bss")]
static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

const SAMPLE_RATE: f32 = 48_000.0;

/// How much of the bell's wet ping-pong to fold on top of the dry chime.
#[cfg(feature = "bell")]
const BELL_DELAY_WET: f32 = 0.6;

/// Master level for the FM bell (dry + its ping-pong), summed onto the bed.
/// 0.02 (≈ −34 dB): a quiet accent under the bed. Well below the limiter
/// ceiling so the bell+voice trails don't cause loud-passage distortion.
#[cfg(feature = "bell")]
const BELL_GAIN: f32 = 0.02;

/// Master level for the pain-material voice, summed onto the bed. 0.04 (≈ −28 dB),
/// a touch above the bell to compensate for the calmer (lower room_size) reverb.
#[cfg(feature = "voice")]
const VOICE_GAIN: f32 = 0.04;

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

// USB capture-tee health (the visualizer feed). Logged per-interval with the
// audio counters. These two exist to settle the open question of WHY the iso
// capture clicks — overflow vs missed 1 ms polls — which our notes say was
// never confirmed (see daisy-usb-capture-clicks):
//   usb_drop      — samples the SAI tee dropped because the iso drain couldn't
//                   keep up (USB_RING full). Counted ONLY while the host is
//                   actively capturing (USB_CAPTURING), so a parked/idle stream
//                   — which intentionally lets the ring overflow — reads clean.
//                   High while capturing ⇒ SD stalls outrun the ~5.3 ms ring
//                   (the "overflow" failure mode; a bigger ring would help).
//   usb_pkt_max_fr — largest single-poll drain in stereo frames the stream task
//                   sent. ~48 is healthy 1 ms pacing; climbing toward the
//                   56-frame packet cap ⇒ the drain is catching up after missed
//                   1 ms polls (the "scheduling" failure mode; only async SD or
//                   a wider poll interval helps — a bigger ring would NOT).
// Read together each beat: drops≈0 + pktmax≈48 = clean; drops spiking = overflow;
// pktmax pinned near the cap with low drops = missed polls.
static USB_DROP: AtomicU32 = AtomicU32::new(0);
static USB_PKT_MAX_FR: AtomicU32 = AtomicU32::new(0);
// True while the host (Pi) has the streaming alt-setting active. Gates USB_DROP
// so the expected overflow during not-capturing periods doesn't inflate it.
static USB_CAPTURING: AtomicBool = AtomicBool::new(false);

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
    info!("ambient-viz-daisy firmware: SD stream + DSP (AXI heap)");

    let board = new_daisy_board!(p);
    let mut led = board.user_led;

    // Debug UART on USART3 TX (D2 / PC10), 115200, read on the Shikra. Only
    // brought up for the `debug-uart` build; the production build leaves USART3
    // and D2 unused and emits nothing.
    #[cfg(feature = "debug-uart")]
    {
        let mut dbg_cfg = embassy_stm32::usart::Config::default();
        dbg_cfg.baudrate = 115_200;
        let dbg_tx =
            embassy_stm32::usart::UartTx::new_blocking(p.USART3, board.pins.d2, dbg_cfg).unwrap();
        debug::init(dbg_tx);
    }
    dbg_uart!("=== ambient-viz-daisy boot ===");

    // Bring up external SDRAM (mapped for the future Engine; not the heap) and
    // init the global heap in AXI SRAM. The heap is on-chip AXI, NOT SDRAM —
    // see HEAP_MEM above. Must precede any allocation.
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
        cm.MPU
            .rasr
            .write((0b011 << 24) | (1 << 17) | (1 << 16) | ((23 - 1) << 1) | 1);
        // Region 2: SRAM1 @ 0x3000_0000, 128 KB, normal non-cacheable
        // (AP=full, TEX=001/C=0/B=0, SIZE=2^17) — keeps SAI DMA coherent.
        cm.MPU.rnr.write(2);
        cm.MPU.rbar.write(0x3000_0000);
        cm.MPU
            .rasr
            .write((0b011 << 24) | (0b001 << 19) | ((17 - 1) << 1) | 1);
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
    // Retry the FAT mount + open the same way as the acquire: a marginal MBR /
    // boot-sector / FAT read on cold boot can fail any of these (blink 2/3) even
    // though the card acquired fine, and a re-read after a short settle recovers
    // without a power cycle. Each step retries independently; the label-break-
    // value keeps the borrow chain (volume <- root <- file) intact on success.
    let volume = 'mount: {
        for _ in 0..5 {
            if let Ok(v) = volume_mgr.open_volume(VolumeIdx(0)) {
                break 'mount v;
            }
            Timer::after_millis(100).await;
        }
        dbg_uart!("SD: FAT volume mount failed after retries (blink 2)");
        blink_code(&mut led, 2).await
    };
    let root = 'root: {
        for _ in 0..5 {
            if let Ok(r) = volume.open_root_dir() {
                break 'root r;
            }
            Timer::after_millis(100).await;
        }
        dbg_uart!("SD: open root dir failed after retries (blink 2)");
        blink_code(&mut led, 2).await
    };
    let file = 'open: {
        for _ in 0..5 {
            if let Ok(f) = root.open_file_in_dir("AMBIENT.RAW", Mode::ReadOnly) {
                break 'open f;
            }
            Timer::after_millis(100).await;
        }
        dbg_uart!("SD: AMBIENT.RAW open failed after retries (blink 3)");
        blink_code(&mut led, 3).await
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
    audio_spawner.must_spawn(audio_task(
        audio_peripherals,
        consumer,
        usb_producer,
        usb_cdc::MIDI_CHANNEL.receiver(),
    ));
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

    // VID 0x1209 = pid.codes (hobby space). PID 0xDA15 instead of the 0x0001
    // "Test PID": 0x0001 is in usb.ids, so Linux/PipeWire names the device
    // "pid.codes Test PID" from the database rather than our product string. An
    // unallocated PID has no usb.ids entry, so the name falls back to `product`.
    let mut dev_cfg = embassy_usb::Config::new(0x1209, 0xDA15);
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

    // CDC ACM in the same composite: full-duplex. Split into a sender
    // (song-position out, Phase C) and a receiver (inbound sensor/freeze MIDI
    // from the Pi → MIDI_CHANNEL → the audio task, Phase E).
    static CDC_STATE: StaticCell<CdcState> = StaticCell::new();
    let cdc = CdcAcmClass::new(&mut usb_builder, CDC_STATE.init(CdcState::new()), 64);
    let (cdc_tx, cdc_rx) = cdc.split();

    let usb_device = usb_builder.build();
    // Keep USB below the audio interrupt executor (P6) so OTG IRQs can't preempt
    // the audio callback mid-write and starve the SAI (glitches). USB tolerates
    // the ~callback-length delay; audio must not.
    interrupt::OTG_FS.set_priority(Priority::P7);
    spawner.must_spawn(usb_audio::usb_task(usb_device));
    spawner.must_spawn(usb_audio::stream_task(uac_audio_ep, usb_consumer));
    spawner.must_spawn(usb_cdc::position_emit_task(cdc_tx, loop_frames));
    spawner.must_spawn(usb_cdc::midi_in_task(cdc_rx));
    dbg_uart!("usb: UAC source + CDC position built + tasks spawned");

    // Chip-temperature telemetry on the thread executor (lowest priority, below
    // the P6 audio interrupt executor), so its ~20 µs ADC busy-wait every 5 s is
    // preempted by the audio callback and never glitches the SAI. Debug-only.
    #[cfg(feature = "debug-uart")]
    spawner.must_spawn(temp::temp_task(p.ADC3));

    // Producer + heartbeat on the thread executor. Blocking SD reads here can
    // no longer glitch the audio — the interrupt executor preempts them.
    let heartbeat = async {
        loop {
            // TEMP DIAG: real-time-health readout on the LED (prod has no UART).
            // PER-INTERVAL, not latching: we read-and-RESET SAI_ERR each beat, so
            // a one-off startup underrun shows fast for a single beat then calms.
            // Fast (150 ms) = the audio callback underran in the LAST interval.
            // Reading: at idle (just the bed) it should settle to the calm 1 s
            // pulse; if it stays fast at idle, even tape+bell overruns. Trigger
            // the voice — if it goes fast only then, the voice is the CPU hog.
            let period = if SAI_ERR.swap(0, Ordering::Relaxed) > 0 {
                150
            } else {
                1000
            };
            led.on();
            Timer::after_millis(period).await;
            led.off();
            Timer::after_millis(period).await;
            // Safe now that dbg_uart writes per-byte (≤87 µs CS, not ~6 ms).
            // cb_full spikes ~730 us on loss-FIR rebuild blocks but they're
            // isolated; sai_err (SAI underruns) is the real health metric.
            dbg_uart!(
                "diag: cb_full {} us | sai_err {} sd_under {} | peak {} | usb_drop {} usb_pktmax {}",
                CB_FULL_US.swap(0, Ordering::Relaxed),
                SAI_ERR.swap(0, Ordering::Relaxed),
                SD_UNDERRUN.swap(0, Ordering::Relaxed),
                OUT_PEAK_MILLI.swap(0, Ordering::Relaxed),
                USB_DROP.swap(0, Ordering::Relaxed),
                USB_PKT_MAX_FR.swap(0, Ordering::Relaxed),
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
    midi_rx: usb_cdc::MidiRx,
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
    // tape -> [freeze send (glitch + return while active)] -> limiter. The
    // freeze stage is compiled in only under the `freeze` feature (off by
    // default); without it the chain is just tape -> limiter. Buffers come from
    // the SDRAM heap.
    let mut tape = TapeProcessor::new(SAMPLE_RATE);
    tape.set_enabled(true);
    #[cfg(feature = "freeze")]
    let mut freeze = Freeze::new(SAMPLE_RATE);
    #[cfg(feature = "freeze")]
    let mut glitch = GlitchTape::new(SAMPLE_RATE);
    let mut limiter = Limiter::new(SAMPLE_RATE);
    // Prime tape's scratch (it resizes on first process()) so the RT callback
    // never allocates.
    tape.process(&mut [0.0f32; HALF_DMA_BUFFER_LENGTH], 0);

    // Bell voice + its ping-pong delay, summed on top of the master pre-limiter
    // (a dry chime over the post-tape backing track). Both are constructed once
    // here — never in the callback — and primed below so the RT path stays
    // alloc-free. FmStab itself allocates nothing (it drives the oscillators via
    // `tick`, not `process`); only the delay's ring buffers hit the heap.
    #[cfg(feature = "bell")]
    let mut bell = {
        let mut b = FmStab::new(SAMPLE_RATE);
        b.load_patch(FmPatch::bell()); // pure-sine FM, ~5 s ring
        b
    };
    #[cfg(feature = "bell")]
    let mut bell_delay = {
        // 0.25 s max ring ≈ 96 KB on the AXI heap (the Engine's 1.0 s default
        // would be ~384 KB and overflow it). `mix = 1.0` → wet-only output; we
        // scale the wet ourselves when summing, like the host Engine's stab bus.
        let mut d = PingPongDelay::new(
            0.25,
            AudioParam::seconds(0.22),
            AudioParam::linear(0.55),
            AudioParam::linear(1.0),
        );
        d.set_sample_rate(SAMPLE_RATE);
        // Prime its internal scratch (resizes on first process()) — the same
        // no-alloc-in-callback discipline as tape above.
        d.process(&mut [0.0f32; HALF_DMA_BUFFER_LENGTH], 0);
        d
    };

    // "Pain material" speech voice — a formant utterance through its own reverb,
    // struck once when the room empties (Pi sends a ch2 note-on). Constructed
    // once here; its SpeechSynth + reverb buffers allocate on the AXI heap now,
    // never in the callback. Idle (silent, skipped) until triggered.
    //
    // Built at the true SAMPLE_RATE (48 kHz) for correct pitch. This used to be
    // forced to 44.1 kHz because `SpeechSynth::new` calls `Stutter::set_sample_rate`,
    // which `Vec::resize`d the Stutter ring — and Vec doubles on grow, so the old
    // 176 KB + new 353 KB coexisted: a ~640 KB transient no AXI heap could hold.
    // The vendored infinitedsp-core patch makes that set_sample_rate a no-op for
    // the ring (see vendor/infinitedsp-core), so 48 kHz now fits and the ~9%
    // pitch hack is gone.
    #[cfg(feature = "voice")]
    let mut voice = PainMaterialVoice::new(SAMPLE_RATE, HALF_DMA_BUFFER_LENGTH);

    let mut sample_index: u64 = 0;
    // CC routing for inbound MIDI (Pi -> tape failure / freeze), shared with the
    // host via install_kiosk_bindings so a CC means the same thing in both.
    let mut midi_map = dsp::MidiMap::new();
    dsp::install_kiosk_bindings(&mut midi_map);
    // Heap high-water: all FX are constructed by now (tape + bell ring + voice
    // reverb), so this is peak allocation. Confirms the 448 KB heap has margin
    // and lets us right-size later. `used`/`free` aren't evaluated in the prod
    // build (dbg_uart! is a no-op without `debug-uart`), so no cost there.
    dbg_uart!(
        "audio: interface started, FX chain ready | heap used {} KB, free {} KB of {} KB",
        HEAP.used() / 1024,
        HEAP.free() / 1024,
        HEAP_SIZE / 1024
    );

    loop {
        // start_callback returns only on a SAI error; on its own executor that
        // shouldn't happen now. Restart rather than panic if it ever does.
        let _ = interface
            .start_callback(|_input, output| {
                let cb_t = embassy_time::Instant::now();
                let n = output.len().min(HALF_DMA_BUFFER_LENGTH);
                let mut buf = [0.0f32; HALF_DMA_BUFFER_LENGTH];
                #[cfg(feature = "freeze")]
                let mut send = [0.0f32; HALF_DMA_BUFFER_LENGTH];
                #[cfg(feature = "bell")]
                let mut bell_send = [0.0f32; HALF_DMA_BUFFER_LENGTH];
                #[cfg(feature = "voice")]
                let mut voice_send = [0.0f32; HALF_DMA_BUFFER_LENGTH];

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

                // Apply inbound CC from the Pi (drained off the channel the CDC
                // read task fills). Decode happened off the RT path; this is just
                // a lookup + setter. CC23 -> tape failure, CC24 -> freeze.
                while let Ok(msg) = midi_rx.try_receive() {
                    match msg {
                        dsp::MidiMessage::ControlChange { cc, value, .. } => {
                            if let Some((param, v)) = midi_map.map_cc(cc, value) {
                                match param {
                                    dsp::Param::TapeFailure => tape.set_failure(v),
                                    #[cfg(feature = "freeze")]
                                    dsp::Param::Freeze => freeze.set_amount(v),
                                    _ => {}
                                }
                            }
                        }
                        // A note-on strikes a foreground voice, routed by MIDI
                        // channel (the Pi/timeline decides when + which):
                        //   ch0 = FM bell, ch1 = industrial stab (shared FmStab
                        //         bank; patch is snapshotted per-voice at strike,
                        //         so swapping it colours only this new note —
                        //         here `note` is the pitch),
                        //   ch2 = speech voice struck on room-empty; here `note`
                        //         selects WHICH phrase (the Pi picks at random),
                        //         pitch is internal to the formant synth.
                        // All mix on top, pre-limiter.
                        #[cfg(any(feature = "bell", feature = "voice"))]
                        dsp::MidiMessage::NoteOn {
                            channel,
                            note,
                            velocity,
                        } => {
                            let v = velocity as f32 / 127.0;
                            match channel {
                                #[cfg(feature = "voice")]
                                2 => voice.trigger_phrase(note as usize, v),
                                #[cfg(feature = "bell")]
                                c => {
                                    bell.load_patch(if c == 1 {
                                        FmPatch::industrial()
                                    } else {
                                        FmPatch::bell()
                                    });
                                    bell.note_on(note, v);
                                }
                                #[cfg(not(feature = "bell"))]
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }

                // Master chain (mirrors Engine::process): tape -> freeze send
                // (glitch + return while active) -> limiter.
                tape.process(&mut buf[..n], sample_index);
                #[cfg(feature = "freeze")]
                {
                    freeze.process(&buf[..n], &mut send[..n]);
                    if freeze.active() {
                        glitch.process(&mut send[..n]);
                        for (o, &g) in buf[..n].iter_mut().zip(send[..n].iter()) {
                            *o += g * freeze::FREEZE_RETURN_GAIN;
                        }
                    }
                }

                // Bell + ping-pong summed on top of the (post-tape) master,
                // before the limiter — so the chime sits over the backing track
                // and the limiter catches the combined peaks. The dry bell goes
                // to both channels; the delay send is left-only so the
                // cross-feedback bounces the echoes L<->R.
                #[cfg(feature = "bell")]
                {
                    let frames = n / 2;
                    for i in 0..frames {
                        // Scale at the source so dry + ping-pong drop together.
                        let s = bell.tick() * BELL_GAIN;
                        buf[2 * i] += s;
                        buf[2 * i + 1] += s;
                        bell_send[2 * i] = s;
                        bell_send[2 * i + 1] = 0.0;
                    }
                    bell_delay.process(&mut bell_send[..n], sample_index);
                    for (o, &w) in buf[..n].iter_mut().zip(bell_send[..n].iter()) {
                        *o += w * BELL_DELAY_WET;
                    }
                }

                // "Pain material" speech (its own reverb) summed on top, same
                // post-fx / pre-limiter slot as the bell. Renders only while
                // sounding — idle blocks are skipped so the reverb costs nothing
                // between utterances.
                #[cfg(feature = "voice")]
                if voice.is_active() {
                    voice.process(&mut voice_send[..n], sample_index);
                    for (o, &w) in buf[..n].iter_mut().zip(voice_send[..n].iter()) {
                        *o += w * VOICE_GAIN;
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
                // Tally tee drops locally and load the capture flag once — keeps
                // the RT path to a single atomic add per block, not per sample.
                let usb_capturing = USB_CAPTURING.load(Ordering::Relaxed);
                let mut usb_dropped = 0u32;
                for (i, frame) in output[..n].chunks_mut(2).enumerate() {
                    let l = buf[2 * i];
                    let r = buf[2 * i + 1];
                    frame[0] = f32_to_u24(l);
                    frame[1] = f32_to_u24(r);
                    if usb_producer.enqueue(f32_to_i16(l)).is_err() {
                        usb_dropped += 1;
                    }
                    if usb_producer.enqueue(f32_to_i16(r)).is_err() {
                        usb_dropped += 1;
                    }
                }
                if usb_capturing && usb_dropped > 0 {
                    USB_DROP.fetch_add(usb_dropped, Ordering::Relaxed);
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
