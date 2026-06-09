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

#[cfg(not(feature = "groovebox"))]
use dsp::{MidiByteParser, MidiMessage};
#[cfg(feature = "groovebox")]
use dsp::GrooveEvent;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver};
use embassy_time::Timer;
use embassy_usb::class::cdc_acm::{Receiver as CdcRx, Sender as CdcTx};
use heapless::String;

use crate::PLAYED_FRAMES;
use crate::usb_audio::{Drv, SAMPLE_RATE_HZ};

/// How often to emit a position line.
const EMIT_PERIOD_MS: u64 = 50;

#[cfg(not(feature = "groovebox"))]
const MIDI_CH_DEPTH: usize = 16;
/// Decoded inbound MIDI, from the CDC read task (thread executor) to the audio
/// task (interrupt executor). Bounded + lock-free `try_send`/`try_receive` so
/// neither side ever blocks the other — the audio side must never block.
#[cfg(not(feature = "groovebox"))]
pub static MIDI_CHANNEL: Channel<CriticalSectionRawMutex, MidiMessage, MIDI_CH_DEPTH> =
    Channel::new();
/// Audio-task end of [`MIDI_CHANNEL`].
#[cfg(not(feature = "groovebox"))]
pub type MidiRx = Receiver<'static, CriticalSectionRawMutex, MidiMessage, MIDI_CH_DEPTH>;

/// Decoded GrooveEvents, from the CDC line-reader task to the groovebox audio
/// task. Same lock-free pattern as [`MIDI_CHANNEL`].
#[cfg(feature = "groovebox")]
pub static GROOVE_CHANNEL: Channel<CriticalSectionRawMutex, GrooveEvent, 16> = Channel::new();
/// Audio-task end of [`GROOVE_CHANNEL`].
#[cfg(feature = "groovebox")]
pub type GrooveRx = Receiver<'static, CriticalSectionRawMutex, GrooveEvent, 16>;

/// Emit song-position lines (device -> host) on the CDC sender half.
#[embassy_executor::task]
pub async fn position_emit_task(mut tx: CdcTx<'static, Drv>, loop_frames: u64) {
    loop {
        tx.wait_connection().await;
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
            // Seconds at millisecond precision, formatted by hand as
            // whole.frac with integer math. Using `{:.3}` on an f32 would drag
            // core::fmt's float path (flt2dec/grisu + f64 soft-float, ~10 KB of
            // flash) into the build — and the H750's internal flash is only
            // 128 KB. Rounded to nearest ms; loop_pos is already < loop_frames
            // so `* 1000` can't overflow u64.
            let sr = SAMPLE_RATE_HZ as u64;
            let total_ms = (loop_pos * 1000 + sr / 2) / sr;
            let whole = total_ms / 1000;
            let millis = total_ms % 1000;

            line.clear();
            // On a loop wrap, prefix RESET so the host hard-snaps rather than
            // interpolating across the seam (complication #10). Same text format
            // as the old `{:.3}` ("12.345"), so the host parser is unchanged.
            let _ = if wrapped {
                write!(line, "RESET {whole}.{millis:03}\nPOS {whole}.{millis:03}\n")
            } else {
                write!(line, "POS {whole}.{millis:03}\n")
            };

            // If the host closed the port, write_packet errors — go back to
            // waiting for a connection rather than blocking (complication #4).
            if tx.write_packet(line.as_bytes()).await.is_err() {
                crate::dbg_uart!("cdc: host closed port");
                break;
            }
        }
    }
}

/// Read inbound CDC bytes, frame them into MIDI messages, and forward to the
/// audio task via [`MIDI_CHANNEL`]. Used by the legacy exhibit pipeline.
#[cfg(not(feature = "groovebox"))]
#[embassy_executor::task]
pub async fn midi_in_task(mut rx: CdcRx<'static, Drv>) {
    let sender = MIDI_CHANNEL.sender();
    let mut parser = MidiByteParser::new();
    let mut buf = [0u8; 64];
    loop {
        rx.wait_connection().await;
        crate::dbg_uart!("cdc: midi-in connected");
        loop {
            match rx.read_packet(&mut buf).await {
                Ok(n) => {
                    for &b in &buf[..n] {
                        if let Some(msg) = parser.push(b) {
                            // Drop if the audio task hasn't drained yet — never
                            // block the USB read on the audio path.
                            let _ = sender.try_send(msg);
                        }
                    }
                }
                Err(_) => break, // disconnected
            }
        }
    }
}

/// Read inbound CDC text lines, parse them as GrooveEvent commands, and forward
/// to the groovebox audio task via [`GROOVE_CHANNEL`]. Used by the groovebox
/// firmware path. The Pi or host sends the same text protocol as the macOS TUI:
///
/// ```text
/// PLAY 1\n   STOP\n   TOGGLE kick 0\n   MACRO filter_cutoff 80\n   …
/// ```
///
/// Line accumulation is done here (off the RT path); the audio task drains the
/// channel per-callback and applies each event to the shared Engine.
#[cfg(feature = "groovebox")]
#[embassy_executor::task]
pub async fn groove_in_task(mut rx: CdcRx<'static, Drv>) {
    let sender = GROOVE_CHANNEL.sender();
    let mut raw = [0u8; 64];
    let mut line: heapless::String<128> = heapless::String::new();
    loop {
        rx.wait_connection().await;
        crate::dbg_uart!("cdc: groove-in connected");
        loop {
            match rx.read_packet(&mut raw).await {
                Ok(n) => {
                    for &b in &raw[..n] {
                        if b == b'\n' || b == b'\r' {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                                if let Ok(evt) = dsp::groove::parse_line(trimmed) {
                                    let _ = sender.try_send(evt);
                                }
                            }
                            line.clear();
                        } else {
                            // Silently drop bytes that overflow the line buffer
                            // (commands longer than 128 bytes are not valid).
                            let _ = line.push(b as char);
                        }
                    }
                }
                Err(_) => break, // disconnected
            }
        }
    }
}
