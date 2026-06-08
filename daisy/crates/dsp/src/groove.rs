//! Shared groovebox control events.
//!
//! Hosts decode their own transport (MIDI, CDC serial, GPIO, I2C, UI) into
//! these compact events, then hand them to `Engine`. Keeping this type in the
//! `no_std` DSP crate gives the macOS host and Daisy firmware one common
//! control vocabulary.

use crate::sequencer;

/// Sequencer lane addressed by a hardware surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    Kick,
    ClosedHat,
    OpenHat,
    Stab,
    Bass,
}

impl Track {
    pub fn id(self) -> u8 {
        match self {
            Track::Kick => 0,
            Track::ClosedHat => 1,
            Track::OpenHat => 2,
            Track::Stab => 3,
            Track::Bass => 4,
        }
    }

    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::Kick),
            1 => Some(Self::ClosedHat),
            2 => Some(Self::OpenHat),
            3 => Some(Self::Stab),
            4 => Some(Self::Bass),
            _ => None,
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "0" | "kick" | "KICK" => Some(Self::Kick),
            "1" | "chat" | "CHAT" | "closed_hat" | "CLOSED_HAT" | "closed" | "CLOSED" => {
                Some(Self::ClosedHat)
            }
            "2" | "ohat" | "OHAT" | "open_hat" | "OPEN_HAT" | "open" | "OPEN" => {
                Some(Self::OpenHat)
            }
            "3" | "stab" | "STAB" => Some(Self::Stab),
            "4" | "bass" | "BASS" => Some(Self::Bass),
            _ => None,
        }
    }

    pub(crate) fn sequencer_voice(self) -> Option<sequencer::Voice> {
        match self {
            Track::Kick => Some(sequencer::Voice::Kick),
            Track::ClosedHat => Some(sequencer::Voice::Chat),
            Track::OpenHat => Some(sequencer::Voice::Ohat),
            Track::Stab => Some(sequencer::Voice::Stab),
            Track::Bass => None,
        }
    }
}

/// Small set of performance macros intended for encoders/faders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Macro {
    /// Tape damage and glitch energy.
    Damage,
    /// Reverb, delay, and bloom send.
    Space,
    /// Shared voice brightness.
    Tone,
    KickLevel,
    HatLevel,
    StabLevel,
    BassLevel,
    /// Standalone Spectre dynamic filter cutoff.
    FilterCutoff,
    /// Standalone Spectre dynamic filter resonance/Q.
    FilterResonance,
    /// Envelope-driven Spectre dynamic filter motion.
    FilterMotion,
}

impl Macro {
    pub fn id(self) -> u8 {
        match self {
            Macro::Damage => 0,
            Macro::Space => 1,
            Macro::Tone => 2,
            Macro::KickLevel => 3,
            Macro::HatLevel => 4,
            Macro::StabLevel => 5,
            Macro::BassLevel => 6,
            Macro::FilterCutoff => 7,
            Macro::FilterResonance => 8,
            Macro::FilterMotion => 9,
        }
    }

    /// Numeric mapping for compact serial protocols.
    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::Damage),
            1 => Some(Self::Space),
            2 => Some(Self::Tone),
            3 => Some(Self::KickLevel),
            4 => Some(Self::HatLevel),
            5 => Some(Self::StabLevel),
            6 => Some(Self::BassLevel),
            7 => Some(Self::FilterCutoff),
            8 => Some(Self::FilterResonance),
            9 => Some(Self::FilterMotion),
            _ => None,
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "0" | "damage" | "DAMAGE" => Some(Self::Damage),
            "1" | "space" | "SPACE" => Some(Self::Space),
            "2" | "tone" | "TONE" => Some(Self::Tone),
            "3" | "kick_level" | "KICK_LEVEL" | "kick" | "KICK" => Some(Self::KickLevel),
            "4" | "hat_level" | "HAT_LEVEL" | "hat" | "HAT" => Some(Self::HatLevel),
            "5" | "stab_level" | "STAB_LEVEL" | "stab" | "STAB" => Some(Self::StabLevel),
            "6" | "bass_level" | "BASS_LEVEL" | "bass" | "BASS" => Some(Self::BassLevel),
            "7" | "filter_cutoff" | "FILTER_CUTOFF" | "cutoff" | "CUTOFF" => {
                Some(Self::FilterCutoff)
            }
            "8" | "filter_resonance" | "FILTER_RESONANCE" | "resonance" | "RESONANCE" | "q"
            | "Q" => Some(Self::FilterResonance),
            "9" | "filter_motion" | "FILTER_MOTION" | "motion" | "MOTION" => {
                Some(Self::FilterMotion)
            }
            _ => None,
        }
    }
}

/// One decoded hardware/UI action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GrooveEvent {
    TransportPlay(bool),
    TransportReset,
    SelectTrack(Track),
    ToggleStep {
        track: Track,
        step: u8,
    },
    SetStepVelocity {
        track: Track,
        step: u8,
        velocity: f32,
    },
    SetMacro {
        macro_id: u8,
        value: f32,
    },
    Pad {
        note: u8,
        velocity: f32,
    },
}

/// Parse errors for the line-oriented groovebox control protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    UnknownCommand,
    MissingField,
    BadBool,
    BadTrack,
    BadMacro,
    BadNumber,
}

/// Parse one line of the shared groovebox control protocol.
///
/// Values are intentionally 7-bit friendly for MIDI/serial bridges:
/// velocities and macro values are `0..127`, normalized to `0.0..1.0`.
///
/// Supported commands:
///
/// ```text
/// PLAY 1
/// RESET
/// TRACK kick
/// PAD 36 127
/// TOGGLE kick 0
/// STEP bass 4 96
/// MACRO damage 64
/// ```
pub fn parse_line(line: &str) -> Result<GrooveEvent, ParseError> {
    let mut parts = line.split_ascii_whitespace();
    let Some(cmd) = parts.next() else {
        return Err(ParseError::Empty);
    };

    match cmd {
        "PLAY" | "play" => Ok(GrooveEvent::TransportPlay(parse_bool(next(&mut parts)?)?)),
        "STOP" | "stop" => Ok(GrooveEvent::TransportPlay(false)),
        "RESET" | "reset" => Ok(GrooveEvent::TransportReset),
        "TRACK" | "track" => Ok(GrooveEvent::SelectTrack(parse_track(next(&mut parts)?)?)),
        "PAD" | "pad" => Ok(GrooveEvent::Pad {
            note: parse_u8(next(&mut parts)?)?,
            velocity: parse_unit(next(&mut parts)?)?,
        }),
        "TOGGLE" | "toggle" => Ok(GrooveEvent::ToggleStep {
            track: parse_track(next(&mut parts)?)?,
            step: parse_u8(next(&mut parts)?)?,
        }),
        "STEP" | "step" => Ok(GrooveEvent::SetStepVelocity {
            track: parse_track(next(&mut parts)?)?,
            step: parse_u8(next(&mut parts)?)?,
            velocity: parse_unit(next(&mut parts)?)?,
        }),
        "MACRO" | "macro" => {
            let m = parse_macro(next(&mut parts)?)?;
            Ok(GrooveEvent::SetMacro {
                macro_id: m.id(),
                value: parse_unit(next(&mut parts)?)?,
            })
        }
        _ => Err(ParseError::UnknownCommand),
    }
}

fn next<'a>(parts: &mut core::str::SplitAsciiWhitespace<'a>) -> Result<&'a str, ParseError> {
    parts.next().ok_or(ParseError::MissingField)
}

fn parse_bool(token: &str) -> Result<bool, ParseError> {
    match token {
        "1" | "on" | "ON" | "true" | "TRUE" => Ok(true),
        "0" | "off" | "OFF" | "false" | "FALSE" => Ok(false),
        _ => Err(ParseError::BadBool),
    }
}

fn parse_track(token: &str) -> Result<Track, ParseError> {
    Track::from_token(token).ok_or(ParseError::BadTrack)
}

fn parse_macro(token: &str) -> Result<Macro, ParseError> {
    Macro::from_token(token).ok_or(ParseError::BadMacro)
}

fn parse_u8(token: &str) -> Result<u8, ParseError> {
    token.parse::<u8>().map_err(|_| ParseError::BadNumber)
}

fn parse_unit(token: &str) -> Result<f32, ParseError> {
    let v = parse_u8(token)?;
    if v > 127 {
        return Err(ParseError::BadNumber);
    }
    Ok(v as f32 / 127.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_transport_and_selection() {
        assert_eq!(parse_line("PLAY 1"), Ok(GrooveEvent::TransportPlay(true)));
        assert_eq!(parse_line("STOP"), Ok(GrooveEvent::TransportPlay(false)));
        assert_eq!(parse_line("RESET"), Ok(GrooveEvent::TransportReset));
        assert_eq!(
            parse_line("TRACK bass"),
            Ok(GrooveEvent::SelectTrack(Track::Bass))
        );
    }

    #[test]
    fn parses_pad_step_toggle_and_macro() {
        assert_eq!(
            parse_line("PAD 36 127"),
            Ok(GrooveEvent::Pad {
                note: 36,
                velocity: 1.0,
            })
        );
        assert_eq!(
            parse_line("TOGGLE kick 7"),
            Ok(GrooveEvent::ToggleStep {
                track: Track::Kick,
                step: 7,
            })
        );
        assert_eq!(
            parse_line("STEP bass 4 64"),
            Ok(GrooveEvent::SetStepVelocity {
                track: Track::Bass,
                step: 4,
                velocity: 64.0 / 127.0,
            })
        );
        assert_eq!(
            parse_line("MACRO damage 32"),
            Ok(GrooveEvent::SetMacro {
                macro_id: Macro::Damage.id(),
                value: 32.0 / 127.0,
            })
        );
        assert_eq!(
            parse_line("MACRO filter_motion 127"),
            Ok(GrooveEvent::SetMacro {
                macro_id: Macro::FilterMotion.id(),
                value: 1.0,
            })
        );
    }

    #[test]
    fn rejects_bad_protocol_lines() {
        assert_eq!(parse_line(""), Err(ParseError::Empty));
        assert_eq!(parse_line("WOBBLE 1"), Err(ParseError::UnknownCommand));
        assert_eq!(parse_line("PLAY maybe"), Err(ParseError::BadBool));
        assert_eq!(parse_line("STEP nope 1 2"), Err(ParseError::BadTrack));
        assert_eq!(parse_line("MACRO nope 1"), Err(ParseError::BadMacro));
        assert_eq!(parse_line("PAD 36 255"), Err(ParseError::BadNumber));
    }
}
