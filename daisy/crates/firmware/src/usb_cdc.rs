//! CDC ACM side of the composite USB device (PLAN_USB_COMPOSITE Phase C).
//!
//! Emits song-position lines so the Pi visualizer can line its JSON lanes up
//! with the audio the Daisy is playing, over the same cable (same USB SOF
//! clock owns both the audio and this serial stream — no cross-cable drift):
//!
//! ```text
//! POS 12.345\n            every 50 ms
//! RESET 0.000\nPOS …      once on each loop wrap (host hard-snaps)
//! ```
//!
//! Position is derived from frames actually rendered by the SAI callback
//! (`crate::PLAYED_FRAMES`), not embassy_time — so it tracks the audio sample
//! clock and can't drift from what's playing.
//!
//! The inbound leg (host -> device sensor-MIDI, Phase E) is deferred: it lands
//! through `dsp::Engine`, which isn't in this firmware yet (it currently streams
//! AMBIENT.RAW straight from SD). When the Engine arrives, split the class and
//! add a `cdc_midi_in_task` reading the bulk-OUT endpoint.

use core::fmt::Write as _;
use core::sync::atomic::Ordering;

use embassy_time::Timer;
use embassy_usb::class::cdc_acm::CdcAcmClass;
use heapless::String;

use crate::PLAYED_FRAMES;
use crate::usb_audio::{Drv, SAMPLE_RATE_HZ};

/// How often to emit a position line.
const EMIT_PERIOD_MS: u64 = 50;

#[embassy_executor::task]
pub async fn position_emit_task(mut cdc: CdcAcmClass<'static, Drv>, loop_frames: u64) {
    loop {
        cdc.wait_connection().await;
        crate::dbg_uart!("cdc: host opened port — emitting POS");

        // Track loop position by accumulating deltas of the (wrapping u32) frame
        // counter, so a counter wrap (~24 h at 48 kHz) never glitches position.
        let mut last = PLAYED_FRAMES.load(Ordering::Relaxed);
        let mut loop_pos: u64 = 0;
        let mut line: String<48> = String::new();

        loop {
            Timer::after_millis(EMIT_PERIOD_MS).await;

            let now = PLAYED_FRAMES.load(Ordering::Relaxed);
            loop_pos += now.wrapping_sub(last) as u64;
            last = now;

            let wrapped = loop_frames != 0 && loop_pos >= loop_frames;
            if wrapped {
                loop_pos %= loop_frames;
            }
            let secs = loop_pos as f32 / SAMPLE_RATE_HZ as f32;

            line.clear();
            // On a loop wrap, prefix RESET so the host hard-snaps rather than
            // interpolating across the seam (complication #10).
            let _ = if wrapped {
                write!(line, "RESET {:.3}\nPOS {:.3}\n", secs, secs)
            } else {
                write!(line, "POS {:.3}\n", secs)
            };

            // If the host closed the port, write_packet errors — go back to
            // waiting for a connection rather than blocking (complication #4).
            if cdc.write_packet(line.as_bytes()).await.is_err() {
                crate::dbg_uart!("cdc: host closed port");
                break;
            }
        }
    }
}
