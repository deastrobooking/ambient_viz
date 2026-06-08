//! Raw-key TUI for interactive groovebox performance.
//!
//! Activated automatically when stdin is a TTY. Translates single-key
//! events into the same `GrooveEvent` path as the text protocol — the
//! engine never knows it's being driven from a key rather than a typed
//! command.
//!
//! Key map:
//!
//! ```text
//! [space]     PLAY / STOP toggle
//! r           RESET (step 0)
//! q / Ctrl+C  quit
//!
//! Track select (affects step-toggle keys below):
//!   k  kick    c  closed_hat   o  open_hat   s  stab   a  bass
//!
//! Step toggle on selected track (steps 0–7):
//!   1 2 3 4 5 6 7 8
//!
//! Pattern bank:
//!   [   prev slot     ]   next slot     p  capture to current slot
//!
//! Macro nudge (~6% per press; uppercase = up, lowercase = down):
//!   d/D  damage        e/E  space (reverb)
//!   f/F  filter_cutoff  m/M  filter_motion
//!
//! ?   show this help
//! ```

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use dsp::{groove::parse_line, GrooveEvent, Macro};

const NUDGE: i32 = 8; // steps out of 0-127 per keypress (~6%)

// ---------------------------------------------------------------------------
// State tracked by the TUI layer (mirrors what the engine would tell us)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct TuiState {
    macros: [f32; 10],
}

impl TuiState {
    fn observe(&mut self, evt: GrooveEvent) {
        if let GrooveEvent::SetMacro { macro_id, value } = evt {
            if let Some(slot) = self.macros.get_mut(macro_id as usize) {
                *slot = value;
            }
        }
    }

    fn macro_val_127(&self, id: u8) -> u8 {
        (self.macros[id as usize] * 127.0).round() as u8
    }

    fn nudge_cmd(&self, name: &str, id: u8, delta: i32) -> String {
        let next = (self.macro_val_127(id) as i32 + delta).clamp(0, 127) as u8;
        format!("MACRO {} {}", name, next)
    }
}

fn track_name(track: dsp::Track) -> &'static str {
    match track {
        dsp::Track::Kick => "kick",
        dsp::Track::ClosedHat => "closed",
        dsp::Track::OpenHat => "open",
        dsp::Track::Stab => "stab",
        dsp::Track::Bass => "bass",
    }
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

fn print_status(state: &TuiState, eng: &dsp::Engine) {
    let seq = eng.sequencer();
    let loop_steps = seq.steps_per_loop().max(1);
    let step = seq.step() as usize % loop_steps;
    let track = eng.selected_track();
    let pat = eng.pattern_bank().selected() + 1;

    let bar: String = (0..loop_steps.min(16))
        .map(|i| if i == step { '>' } else { '·' })
        .collect();

    print!(
        "\r\x1B[2K[{play}] trk:{track:<5} pat:{pat}/8  {bar}  \
         dmg:{dmg:3} spc:{spc:3} fcut:{fcut:3} fmot:{fmot:3}  ? help",
        play = if eng.sequencer_enabled() { "PLAY" } else { "STOP" },
        track = track_name(track),
        bar = bar,
        dmg = state.macro_val_127(Macro::Damage.id()),
        spc = state.macro_val_127(Macro::Space.id()),
        fcut = state.macro_val_127(Macro::FilterCutoff.id()),
        fmot = state.macro_val_127(Macro::FilterMotion.id()),
    );
    io::stdout().flush().ok();
}

fn print_tui_help() {
    println!("\r");
    println!("\r  Groovebox TUI keys:");
    println!("\r  [space] PLAY/STOP   r RESET   q quit");
    println!("\r  k kick  c closed_hat  o open_hat  s stab  a bass  — track select");
    println!("\r  1-8  toggle steps 0-7 on selected track");
    println!("\r  [ prev pattern   ] next pattern   p capture to slot");
    println!("\r  d/D damage-/+   e/E space-/+   f/F filter_cutoff-/+   m/M filter_motion-/+");
    println!("\r");
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

/// Block-run the TUI on the calling thread. Exits cleanly on `q` or Ctrl+C,
/// restoring the terminal before returning.
pub fn run(engine: Arc<Mutex<dsp::Engine>>) {
    enable_raw_mode().expect("crossterm: enable raw mode");
    print_tui_help();

    let mut state = TuiState::default();

    {
        let eng = engine.lock().unwrap();
        print_status(&state, &eng);
    }

    loop {
        let Ok(event) = event::read() else { break };

        let Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        }) = event
        else {
            continue;
        };
        if kind != KeyEventKind::Press {
            continue;
        }

        let cmd: Option<String> = match (code, modifiers) {
            // ── quit ────────────────────────────────────────────────────────
            (KeyCode::Char('q'), _) => break,
            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => break,

            // ── transport ───────────────────────────────────────────────────
            (KeyCode::Char(' '), _) => {
                let playing = engine.lock().unwrap().sequencer_enabled();
                Some(if playing { "STOP".into() } else { "PLAY 1".into() })
            }
            (KeyCode::Char('r'), _) => Some("RESET".into()),

            // ── track select ────────────────────────────────────────────────
            (KeyCode::Char('k'), _) => Some("TRACK kick".into()),
            (KeyCode::Char('c'), _) => Some("TRACK closed".into()),
            (KeyCode::Char('o'), _) => Some("TRACK open".into()),
            (KeyCode::Char('s'), _) => Some("TRACK stab".into()),
            (KeyCode::Char('a'), _) => Some("TRACK bass".into()),

            // ── step toggle: keys 1-8 → steps 0-7 ──────────────────────────
            (KeyCode::Char(n @ '1'..='8'), _) => {
                let step = n as u8 - b'1';
                let t = track_name(engine.lock().unwrap().selected_track());
                Some(format!("TOGGLE {} {}", t, step))
            }

            // ── pattern bank ────────────────────────────────────────────────
            (KeyCode::Char('['), _) => {
                let slot = engine.lock().unwrap().pattern_bank().selected();
                if slot > 0 {
                    Some(format!("PATTERN {}", slot)) // slot is 0-indexed; cmd is 1-indexed
                } else {
                    None
                }
            }
            (KeyCode::Char(']'), _) => {
                let slot = engine.lock().unwrap().pattern_bank().selected();
                if slot < dsp::PATTERN_BANK_SLOTS - 1 {
                    Some(format!("PATTERN {}", slot + 2))
                } else {
                    None
                }
            }
            (KeyCode::Char('p'), _) => {
                let slot = engine.lock().unwrap().pattern_bank().selected();
                Some(format!("CAPTURE {}", slot + 1))
            }

            // ── macro nudge (lowercase = down, uppercase = up) ───────────────
            (KeyCode::Char('d'), _) => Some(state.nudge_cmd("damage", Macro::Damage.id(), -NUDGE)),
            (KeyCode::Char('D'), _) => Some(state.nudge_cmd("damage", Macro::Damage.id(), NUDGE)),
            (KeyCode::Char('e'), _) => Some(state.nudge_cmd("space", Macro::Space.id(), -NUDGE)),
            (KeyCode::Char('E'), _) => Some(state.nudge_cmd("space", Macro::Space.id(), NUDGE)),
            (KeyCode::Char('f'), _) => {
                Some(state.nudge_cmd("filter_cutoff", Macro::FilterCutoff.id(), -NUDGE))
            }
            (KeyCode::Char('F'), _) => {
                Some(state.nudge_cmd("filter_cutoff", Macro::FilterCutoff.id(), NUDGE))
            }
            (KeyCode::Char('m'), _) => {
                Some(state.nudge_cmd("filter_motion", Macro::FilterMotion.id(), -NUDGE))
            }
            (KeyCode::Char('M'), _) => {
                Some(state.nudge_cmd("filter_motion", Macro::FilterMotion.id(), NUDGE))
            }

            // ── help ────────────────────────────────────────────────────────
            (KeyCode::Char('?'), _) | (KeyCode::Char('h'), _) => {
                print_tui_help();
                None
            }

            _ => None,
        };

        if let Some(ref cmd_str) = cmd {
            match parse_line(cmd_str) {
                Ok(evt) => {
                    engine.lock().unwrap().handle_groove_event(evt);
                    state.observe(evt);
                }
                Err(e) => {
                    print!("\r\x1B[2K  parse error {:?}: {}", e, cmd_str);
                    io::stdout().flush().ok();
                    continue;
                }
            }
        }

        let eng = engine.lock().unwrap();
        print_status(&state, &eng);
    }

    disable_raw_mode().ok();
    println!("\r");
}
