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

    let p = embassy_stm32::init(Default::default());
    info!("ambient-viz-daisy firmware: hello");

    let board = new_daisy_board!(p);
    let mut led = board.user_led;

    // NOTE: the SD stack and dsp engine are intentionally NOT constructed at
    // boot yet — both run before the loop and panic on real hardware, which is
    // why an earlier build flashed fine but never blinked:
    //   - sd::build_sd_card() inits SPI1, whose kernel clock the Default::default()
    //     RCC config doesn't set up — needs daisy_embassy::default_rcc().
    //   - dsp::Engine::new() allocates reverb/delay buffers that exceed the 64 KB
    //     SRAM heap — needs to move to SDRAM (see BREAKOUT.md + memory note).
    // They still compile-check (`mod sd;` + the dsp dep). Re-enable during the
    // audio/SD bring-up phase, after the RCC + SDRAM work:
    //
    //   let _sdcard = sd::build_sd_card(
    //       p.SPI1, board.pins.d8, board.pins.d10, board.pins.d9, board.pins.d7);
    //   let _engine = dsp::Engine::new(48_000.0);

    loop {
        led.on();
        Timer::after_millis(500).await;
        led.off();
        Timer::after_millis(500).await;
    }
}
