#![no_std]
#![no_main]

mod debug;
mod sd;
#[allow(dead_code)] // some control-handler accessors unused until composite CDC
mod uac_source;
mod usb_audio;
mod usb_cdc;

use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, Ordering};

use daisy_embassy::audio::AudioPeripherals;
use daisy_embassy::led::UserLed;
use daisy_embassy::{hal, new_daisy_board};
use defmt::info;
use embassy_executor::{InterruptExecutor, Spawner};
use embassy_futures::join::join;
use embassy_futures::yield_now;
use embassy_stm32::interrupt;
use embassy_stm32::interrupt::{InterruptExt, Priority};
use embassy_time::Timer;
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
const HEAP_SIZE: usize = 64 * 1024;
static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

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
    unsafe {
        let ptr = (&raw mut HEAP_MEM) as *mut u8;
        HEAP.init(ptr as usize, HEAP_SIZE);
    }

    let p = hal::init(daisy_embassy::default_rcc());
    info!("ambient-viz-daisy firmware: SD stream (2b, interrupt executor)");

    let board = new_daisy_board!(p);
    let mut led = board.user_led;

    // Debug UART on USART3 TX (D2 / PC10), 115200, read on the Shikra.
    let mut dbg_cfg = embassy_stm32::usart::Config::default();
    dbg_cfg.baudrate = 115_200;
    let dbg_tx =
        embassy_stm32::usart::UartTx::new_blocking(p.USART3, board.pins.d2, dbg_cfg).unwrap();
    debug::init(dbg_tx);
    dbg_uart!("=== ambient-viz-daisy boot: SD stream (2b) ===");

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
    dbg_uart!("audio: interface started");
    loop {
        // start_callback returns only on a SAI error; on its own executor that
        // shouldn't happen now. Restart rather than panic if it ever does.
        let _ = interface
            .start_callback(|_input, output| {
                for frame in output.chunks_mut(2) {
                    let l = consumer.dequeue().unwrap_or(0);
                    let r = consumer.dequeue().unwrap_or(0);
                    frame[0] = i16_to_u24(l);
                    frame[1] = i16_to_u24(r);
                    // Tee what we play to the USB capture ring. Drops on full,
                    // which is exactly when the host isn't draining (not capturing).
                    let _ = usb_producer.enqueue(l);
                    let _ = usb_producer.enqueue(r);
                }
                // Advance the play position by the frames rendered this block.
                PLAYED_FRAMES.fetch_add((output.len() / 2) as u32, Ordering::Relaxed);
            })
            .await;
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

/// i16 sample -> 24-bit signed (low bits of a u32), the SAI callback's format.
#[inline(always)]
fn i16_to_u24(s: i16) -> u32 {
    ((s as i32) << 8) as u32
}
