#![no_std]
#![no_main]

mod sd;

use core::mem::MaybeUninit;

use daisy_embassy::led::UserLed;
use daisy_embassy::{hal, new_daisy_board};
use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_futures::yield_now;
use embassy_time::Timer;
use embedded_alloc::LlffHeap as Heap;
use embedded_sdmmc::{Mode, VolumeIdx, VolumeManager};
use heapless::spsc::Queue;
use static_cell::StaticCell;

use {defmt_rtt as _, panic_probe as _};

#[global_allocator]
static HEAP: Heap = Heap::empty();
const HEAP_SIZE: usize = 64 * 1024;
static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

const RING_LEN: usize = 8192;
static RING: StaticCell<Queue<i16, RING_LEN>> = StaticCell::new();

// Streaming step 2a (cooperative) with full LED diagnostics:
//   1 flash + pause   = no card / SPI init failed
//   2 flashes + pause = FAT volume / root dir mount failed
//   3 flashes + pause = AMBIENT.RAW open failed
//   steady 1 Hz blink = reached the streaming loop (SD setup OK) — audio is
//                       the success signal from here; silence + heartbeat means
//                       a format / ring / audio-path problem, not SD setup
//   dark              = panicked somewhere unexpected (e.g. audio start error)

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    unsafe {
        let ptr = (&raw mut HEAP_MEM) as *mut u8;
        HEAP.init(ptr as usize, HEAP_SIZE);
    }

    let p = hal::init(daisy_embassy::default_rcc());
    info!("ambient-viz-daisy firmware: SD stream (2a, diagnostics)");

    let board = new_daisy_board!(p);
    let mut led = board.user_led;
    let interface = board
        .audio_peripherals
        .prepare_interface(Default::default())
        .await;

    let sdcard = sd::build_sd_card(
        p.SPI1,
        board.pins.d8,  // PG11 / SCK
        board.pins.d10, // PB5  / MOSI
        board.pins.d9,  // PB4  / MISO
        board.pins.d7,  // PG10 / CS
    );

    if sdcard.num_bytes().is_err() {
        info!("SD: no card / init failed");
        blink_code(&mut led, 1).await;
    }

    let volume_mgr = VolumeManager::new(sdcard, sd::ZeroTime);
    let volume = match volume_mgr.open_volume(VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => {
            info!("SD: FAT volume mount failed");
            blink_code(&mut led, 2).await
        }
    };
    let root = match volume.open_root_dir() {
        Ok(r) => r,
        Err(_) => {
            info!("SD: open root dir failed");
            blink_code(&mut led, 2).await
        }
    };
    let file = match root.open_file_in_dir("AMBIENT.RAW", Mode::ReadOnly) {
        Ok(f) => f,
        Err(_) => {
            info!("SD: AMBIENT.RAW open failed");
            blink_code(&mut led, 3).await
        }
    };
    info!("SD: streaming AMBIENT.RAW ({} bytes)", file.length());

    let q = RING.init(Queue::new());
    let (mut producer, mut consumer) = q.split();

    // Heartbeat: shows we reached streaming (vs a panic = dark LED).
    let heartbeat = async {
        loop {
            led.on();
            Timer::after_millis(500).await;
            led.off();
            Timer::after_millis(500).await;
        }
    };

    // Producer: read AMBIENT.RAW one block at a time into the ring, looping at
    // EOF; yield between blocks and when full so the audio callback runs.
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

    // Consumer: drain L,R pairs into each output frame; silence on underrun.
    // start_callback only returns on a SAI error (e.g. an overrun when a
    // blocking SD read on this same executor stalls past the refill deadline).
    // Don't panic on that — restart the callback so the stream continues with
    // a glitch at the seam instead of dying. (Step 2b removes the overruns by
    // moving this onto a high-priority interrupt executor.)
    let audio = async {
        let mut interface = unwrap!(interface.start_interface().await.ok());
        loop {
            let _ = interface
                .start_callback(|_input, output| {
                    for frame in output.chunks_mut(2) {
                        let l = consumer.dequeue().unwrap_or(0);
                        let r = consumer.dequeue().unwrap_or(0);
                        frame[0] = i16_to_u24(l);
                        frame[1] = i16_to_u24(r);
                    }
                })
                .await;
            yield_now().await;
        }
    };

    join3(heartbeat, reader, audio).await;
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
