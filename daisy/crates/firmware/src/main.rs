#![no_std]
#![no_main]

mod sd;

use core::mem::MaybeUninit;

use daisy_embassy::new_daisy_board;
use defmt::info;
use embassy_executor::Spawner;
use embassy_time::Timer;
use embedded_alloc::LlffHeap as Heap;
use embedded_sdmmc::{Mode, VolumeIdx, VolumeManager};

use {defmt_rtt as _, panic_probe as _};

#[global_allocator]
static HEAP: Heap = Heap::empty();

/// 64 KB heap in SRAM. Holds the dsp reverb's internal buffers + the dry
/// scratch buffer. Sized generously; bump if other dsp modules are added.
const HEAP_SIZE: usize = 64 * 1024;
static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // Initialise the global allocator before anything that might allocate.
    unsafe {
        let ptr = (&raw mut HEAP_MEM) as *mut u8;
        HEAP.init(ptr as usize, HEAP_SIZE);
    }

    // Daisy clock tree (480 MHz core + peripheral kernel clocks). Needed so
    // SPI1 has a kernel clock — Default::default() leaves it unset and the SD
    // SPI init panics (that's why an earlier build flashed but never blinked).
    let p = embassy_stm32::init(daisy_embassy::default_rcc());
    info!("ambient-viz-daisy firmware: hello");

    let board = new_daisy_board!(p);
    let mut led = board.user_led;

    // Bring up the SD card on SPI1 (see crates/firmware/src/sd.rs).
    let sdcard = sd::build_sd_card(
        p.SPI1,
        board.pins.d8,  // PG11 / SCK
        board.pins.d10, // PB5  / MOSI
        board.pins.d9,  // PB4  / MISO
        board.pins.d7,  // PG10 / CS
    );

    // Walk the SD bring-up stages and record how far we got. No debug probe is
    // attached, so the result is reported as an LED blink code below:
    //   0 = success: FAT32 mounted, AMBIENT.RAW opened + read
    //   1 = no card / SPI init failed
    //   2 = card OK but FAT32 volume / root dir mount failed
    //   3 = volume OK but AMBIENT.RAW not found / unreadable
    let status: u8 = if sdcard.num_bytes().is_err() {
        info!("SD: no card / init failed");
        1
    } else {
        // VolumeManager takes ownership of the card; num_bytes()'s borrow is
        // already released by here.
        let volume_mgr = VolumeManager::new(sdcard, sd::ZeroTime);
        match volume_mgr.open_volume(VolumeIdx(0)) {
            Err(_) => {
                info!("SD: FAT volume mount failed");
                2
            }
            Ok(volume) => match volume.open_root_dir() {
                Err(_) => {
                    info!("SD: open root dir failed");
                    2
                }
                Ok(root) => match root.open_file_in_dir("AMBIENT.RAW", Mode::ReadOnly) {
                    Err(_) => {
                        info!("SD: AMBIENT.RAW not found / open failed");
                        3
                    }
                    Ok(file) => {
                        let mut buf = [0u8; 512];
                        match file.read(&mut buf) {
                            Ok(n) if n > 0 => {
                                info!("SD: AMBIENT.RAW opened, read {} bytes", n);
                                0
                            }
                            _ => {
                                info!("SD: AMBIENT.RAW read returned no data");
                                3
                            }
                        }
                    }
                },
            },
        }
    };

    // dsp::Engine::new() still deferred — its reverb/delay buffers overflow the
    // 64 KB SRAM heap (needs SDRAM; see BREAKOUT.md + memory note):
    //   let _engine = dsp::Engine::new(48_000.0);

    // Blink code: steady 1 Hz on success (status 0), else `status` quick
    // flashes followed by a pause, repeating.
    loop {
        if status == 0 {
            led.on();
            Timer::after_millis(500).await;
            led.off();
            Timer::after_millis(500).await;
        } else {
            for _ in 0..status {
                led.on();
                Timer::after_millis(150).await;
                led.off();
                Timer::after_millis(150).await;
            }
            Timer::after_millis(1000).await;
        }
    }
}
