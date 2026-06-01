#![no_std]
#![no_main]

mod sd;

use core::mem::MaybeUninit;

use daisy_embassy::audio::HALF_DMA_BUFFER_LENGTH;
use daisy_embassy::{hal, new_daisy_board};
use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_time::Timer;
use embedded_alloc::LlffHeap as Heap;

use {defmt_rtt as _, panic_probe as _};

// Global allocator kept for the dsp dependency even though the engine isn't
// constructed yet (its reverb/delay buffers overflow this 64 KB SRAM heap —
// needs SDRAM; deferred to the audio-DSP phase).
#[global_allocator]
static HEAP: Heap = Heap::empty();

const HEAP_SIZE: usize = 64 * 1024;
static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

// Audio-path bring-up (streaming step 1 of 2): emit a fixed test tone on the
// stereo line out to validate SAI + PCM3060 + the jack + the monitor chain,
// independent of SD. Step 2 replaces the tone with samples streamed from
// AMBIENT.RAW via a ring buffer (mod sd; still compile-checks the read path).

/// ~440 Hz at the 48 kHz codec rate: 48000 / 440 ≈ 109 samples per period.
const PERIOD: u32 = 109;
/// Output level, [0,1]. Keep the line out civilised.
const AMPLITUDE: f32 = 0.2;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    unsafe {
        let ptr = (&raw mut HEAP_MEM) as *mut u8;
        HEAP.init(ptr as usize, HEAP_SIZE);
    }

    let p = hal::init(daisy_embassy::default_rcc());
    info!("ambient-viz-daisy firmware: audio tone test");

    let board = new_daisy_board!(p);
    let mut led = board.user_led;

    let interface = board
        .audio_peripherals
        .prepare_interface(Default::default())
        .await;

    let mut buf = [0u32; HALF_DMA_BUFFER_LENGTH];
    let mut pos: u32 = 0;

    // 1 Hz heartbeat — distinguishes "running but silent" (audio-path problem)
    // from "dark" (panic/crash).
    let led_fut = async {
        loop {
            led.on();
            Timer::after_millis(500).await;
            led.off();
            Timer::after_millis(500).await;
        }
    };

    // Fill every output block with the test tone (same sample on L and R).
    let audio_fut = async {
        let mut interface = unwrap!(interface.start_interface().await);
        unwrap!(
            interface
                .start_callback(|_input, output| {
                    for frame in buf.chunks_mut(2) {
                        let s = f32_to_u24(make_triangle(pos % PERIOD, PERIOD) * AMPLITUDE);
                        frame[0] = s; // L
                        frame[1] = s; // R
                        pos = pos.wrapping_add(1);
                    }
                    output.copy_from_slice(&buf);
                })
                .await
        );
    };

    join(led_fut, audio_fut).await;
}

/// Triangle wave in [-1.0, 1.0] for `pos` in [0, period].
fn make_triangle(pos: u32, period: u32) -> f32 {
    if pos <= period / 2 {
        pos as f32 * 4.0 / period as f32 - 1.0
    } else {
        let pos = pos - period / 2;
        pos as f32 * -4.0 / period as f32 + 1.0
    }
}

/// f32 [-1.0, 1.0] -> 24-bit signed sample (stored in the low bits of a u32),
/// the format the daisy-embassy SAI callback expects.
#[inline(always)]
fn f32_to_u24(x: f32) -> u32 {
    let x = (x * 8_388_607.0).clamp(-8_388_608.0, 8_388_607.0);
    (x as i32) as u32
}
