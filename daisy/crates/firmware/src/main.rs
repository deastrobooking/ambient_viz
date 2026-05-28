#![no_std]
#![no_main]

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
