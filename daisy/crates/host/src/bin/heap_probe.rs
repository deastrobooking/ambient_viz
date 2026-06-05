//! Heap-demand probe for the Daisy firmware's FX chain — runs on the Mac.
//!
//! The allocation SIZES are identical on host and Daisy (f32 = 4 B both, same
//! `Vec` doubling-on-grow, same construction order). Only the budget differs:
//! the Daisy heap is 448 KB, the Mac has gigabytes. So we reproduce the Daisy's
//! heap demand here by wrapping the allocator and recording the PEAK bytes
//! outstanding while building the FULL prod FX chain (tape + limiter + bell +
//! voice) in the firmware's exact order — then compare to the on-device heap.
//!
//! `cargo run -p host --bin heap_probe`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use dsp::limiter::Limiter;
use dsp::tape::TapeProcessor;
use dsp::{AudioParam, FmPatch, FmStab, FrameProcessor, PainMaterialVoice, PingPongDelay};

struct Track;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Track {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = CUR.fetch_add(l.size(), SeqCst) + l.size();
            PEAK.fetch_max(now, SeqCst);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        CUR.fetch_sub(l.size(), SeqCst);
        unsafe { System.dealloc(p, l) };
    }
}

#[global_allocator]
static A: Track = Track;

const SR: f32 = 48_000.0;
const BLK: usize = 64; // HALF_DMA_BUFFER_LENGTH

fn kb(b: usize) -> usize {
    b / 1024
}

fn verdict(peak_over_base: usize) -> &'static str {
    let k = kb(peak_over_base);
    if k > 512 {
        "OVERFLOWS even a maxed AXI heap (512 KB)"
    } else if k > 448 {
        "OVERFLOWS 448 KB (fits a maxed ~504 KB heap)"
    } else {
        "fits 448 KB"
    }
}

fn main() {
    println!("=== Daisy FULL prod FX heap-demand probe (host) ===\n");
    let base = CUR.load(SeqCst);

    // --- firmware audio_task construction order ---------------------------
    let mut tape = TapeProcessor::new(SR);
    tape.set_enabled(true);
    tape.process(&mut [0.0f32; BLK], 0);
    let after_tape = CUR.load(SeqCst);

    let _limiter = Limiter::new(SR);
    let after_lim = CUR.load(SeqCst);

    // bell: FmStab + ping-pong delay (the 88 KB ring), primed.
    let mut bell = FmStab::new(SR);
    bell.load_patch(FmPatch::bell());
    let mut bell_delay = PingPongDelay::new(
        0.25,
        AudioParam::seconds(0.22),
        AudioParam::linear(0.55),
        AudioParam::linear(1.0),
    );
    bell_delay.set_sample_rate(SR);
    bell_delay.process(&mut [0.0f32; BLK], 0);
    let after_bell = CUR.load(SeqCst);

    println!("baseline (std):                {} KB", kb(base));
    println!("tape + prime:                 +{} KB", kb(after_tape - base));
    println!("limiter:                      +{} KB", kb(after_lim - after_tape));
    println!("bell + ping-pong ring:        +{} KB", kb(after_bell - after_lim));
    println!("--- resident before voice:     {} KB ---\n", kb(after_bell - base));

    // voice @ 44.1 kHz (the firmware's current choice — should skip the Stutter resize)
    PEAK.store(CUR.load(SeqCst), SeqCst);
    let v44 = PainMaterialVoice::new(48_000.0, BLK);
    let steady = CUR.load(SeqCst);
    let peak = PEAK.load(SeqCst);
    println!("voice @ 44.1 kHz (bell+voice prod):");
    println!("  voice resident:             +{} KB", kb(steady - after_bell));
    println!("  peak spike during build:     {} KB (over pre-voice)", kb(peak - after_bell));
    println!("  >> TOTAL heap peak:           {} KB", kb(peak - base));
    println!("  >> VERDICT:                   {}", verdict(peak - base));
    drop(v44);

    println!("\n(64-bit host metadata is a bit larger than 32-bit Daisy, but the");
    println!(" f32 audio buffers that dominate are identical sizes.)");
}
