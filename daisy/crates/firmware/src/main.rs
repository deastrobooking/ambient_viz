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

    // SD card stack — built but not driven yet. Constructing the SdCard
    // proves the dep graph + pin assignments + bus-adapter chain all
    // type-check before the physical breakout exists. Actual num_bytes()
    // or VolumeManager calls would block waiting for hardware, so they're
    // deferred to bring-up. See crates/firmware/src/sd.rs.
    let _sdcard = sd::build_sd_card(
        p.SPI1,
        board.pins.d8,  // PG11 / SCK
        board.pins.d10, // PB5  / MOSI
        board.pins.d9,  // PB4  / MISO
        board.pins.d7,  // PG10 / CS (software-driven GPIO)
    );

    // Construct the shared dsp engine — same code path as the Mac host uses.
    // Sample rate is hard-coded for now; will be wired to the SAI config in
    // the next phase (audio passthrough).
    let _engine = dsp::Engine::new(48_000.0);

    loop {
        led.on();
        Timer::after_millis(500).await;
        led.off();
        Timer::after_millis(500).await;
    }
}
