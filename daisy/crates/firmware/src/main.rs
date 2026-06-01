#![no_std]
#![no_main]

mod sd;

use core::mem::MaybeUninit;

use daisy_embassy::new_daisy_board;
use defmt::info;
use embassy_executor::Spawner;
use embassy_time::Timer;
use embedded_alloc::LlffHeap as Heap;

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

    // num_bytes() triggers card initialisation: Ok = card present + responding,
    // Err = no card / init failed. No debug probe is attached, so report the
    // result on the user LED — 1 Hz = SD OK, 4 Hz = no card / failure.
    let sd_ok = sdcard.num_bytes().is_ok();
    if sd_ok {
        info!("SD: card initialised");
    } else {
        info!("SD: no card / init failed");
    }
    let half_period_ms: u64 = if sd_ok { 500 } else { 125 };

    // dsp::Engine::new() still deferred — its reverb/delay buffers overflow the
    // 64 KB SRAM heap (needs SDRAM; see BREAKOUT.md + memory note):
    //   let _engine = dsp::Engine::new(48_000.0);

    loop {
        led.on();
        Timer::after_millis(half_period_ms).await;
        led.off();
        Timer::after_millis(half_period_ms).await;
    }
}
