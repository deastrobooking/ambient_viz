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

/// Dynamic filter parameter addressed by the shared control protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterParam {
    Cutoff,
    Resonance,
    Motion,
}

impl FilterParam {
    pub fn id(self) -> u8 {
        match self {
            FilterParam::Cutoff => 0,
            FilterParam::Resonance => 1,
            FilterParam::Motion => 2,
        }
    }

    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::Cutoff),
            1 => Some(Self::Resonance),
            2 => Some(Self::Motion),
            _ => None,
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "0" | "cutoff" | "CUTOFF" | "freq" | "FREQ" | "frequency" | "FREQUENCY" => {
                Some(Self::Cutoff)
            }
            "1" | "resonance" | "RESONANCE" | "q" | "Q" => Some(Self::Resonance),
            "2" | "motion" | "MOTION" | "dynamic" | "DYNAMIC" => Some(Self::Motion),
            _ => None,
        }
    }
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
    /// Load/select a zero-based pattern bank slot.
    SelectPattern(u8),
    /// Capture the current sequencer pattern into a zero-based bank slot.
    CapturePattern(u8),
    CopyPattern {
        src: u8,
        dst: u8,
    },
    ClearPattern(u8),
    FillPattern {
        slot: u8,
        track: Track,
        velocity: f32,
    },
    RandomizePattern {
        slot: u8,
        track: Track,
        seed: u8,
        density: f32,
        velocity: f32,
    },
    /// Select a zero-based Spectre dynamic filter band index.
    SelectFilterBand(u8),
    ToggleStep {
        track: Track,
        step: u8,
    },
    SetStepVelocity {
        track: Track,
        step: u8,
        velocity: f32,
    },
    SetBassStep {
        slot: Option<u8>,
        step: u8,
        cell: sequencer::BassCell,
    },
    SetMacro {
        macro_id: u8,
        value: f32,
    },
    /// Set a Spectre filter parameter. `None` means use the selected band.
    SetFilterParam {
        band: Option<u8>,
        param: FilterParam,
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
    BadFilterBand,
    BadFilterParam,
    BadPatternSlot,
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
/// BASS 4 hold
/// PBASS 1 4 rest
/// MACRO damage 64
/// PATTERN 1
/// CAPTURE 1
/// PCOPY 1 2
/// PCLEAR 2
/// PFILL 1 kick 127
/// PRAND 1 kick 42 64 127
/// BAND 1
/// FILTER cutoff 80
/// FILTER 3 q 48
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
        "PATTERN" | "pattern" => Ok(GrooveEvent::SelectPattern(parse_pattern_slot(next(
            &mut parts,
        )?)?)),
        "CAPTURE" | "capture" => Ok(GrooveEvent::CapturePattern(parse_pattern_slot(next(
            &mut parts,
        )?)?)),
        "PCOPY" | "pcopy" => Ok(GrooveEvent::CopyPattern {
            src: parse_pattern_slot(next(&mut parts)?)?,
            dst: parse_pattern_slot(next(&mut parts)?)?,
        }),
        "PCLEAR" | "pclear" => Ok(GrooveEvent::ClearPattern(parse_pattern_slot(next(
            &mut parts,
        )?)?)),
        "PFILL" | "pfill" => Ok(GrooveEvent::FillPattern {
            slot: parse_pattern_slot(next(&mut parts)?)?,
            track: parse_track(next(&mut parts)?)?,
            velocity: parse_unit(next(&mut parts)?)?,
        }),
        "PRAND" | "prand" => Ok(GrooveEvent::RandomizePattern {
            slot: parse_pattern_slot(next(&mut parts)?)?,
            track: parse_track(next(&mut parts)?)?,
            seed: parse_u8(next(&mut parts)?)?,
            density: parse_unit(next(&mut parts)?)?,
            velocity: parse_unit(next(&mut parts)?)?,
        }),
        "BAND" | "band" => Ok(GrooveEvent::SelectFilterBand(parse_filter_band(next(
            &mut parts,
        )?)?)),
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
        "BASS" | "bass" => Ok(GrooveEvent::SetBassStep {
            slot: None,
            step: parse_u8(next(&mut parts)?)?,
            cell: parse_bass_cell(next(&mut parts)?)?,
        }),
        "PBASS" | "pbass" => Ok(GrooveEvent::SetBassStep {
            slot: Some(parse_pattern_slot(next(&mut parts)?)?),
            step: parse_u8(next(&mut parts)?)?,
            cell: parse_bass_cell(next(&mut parts)?)?,
        }),
        "MACRO" | "macro" => {
            let m = parse_macro(next(&mut parts)?)?;
            Ok(GrooveEvent::SetMacro {
                macro_id: m.id(),
                value: parse_unit(next(&mut parts)?)?,
            })
        }
        "FILTER" | "filter" => parse_filter_command(&mut parts),
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

fn parse_filter_param(token: &str) -> Result<FilterParam, ParseError> {
    FilterParam::from_token(token).ok_or(ParseError::BadFilterParam)
}

fn parse_bass_cell(token: &str) -> Result<sequencer::BassCell, ParseError> {
    match token {
        "rest" | "REST" | "off" | "OFF" | "." | "-" | "0" => Ok(sequencer::BassCell::Rest),
        "hold" | "HOLD" | "tie" | "TIE" | "_" => Ok(sequencer::BassCell::Hold),
        _ => Ok(sequencer::BassCell::Strike(parse_unit(token)?)),
    }
}

fn parse_filter_band(token: &str) -> Result<u8, ParseError> {
    let band = parse_u8(token)?;
    if !(1..=8).contains(&band) {
        return Err(ParseError::BadFilterBand);
    }
    Ok(band - 1)
}

fn parse_pattern_slot(token: &str) -> Result<u8, ParseError> {
    let slot = parse_u8(token)?;
    if !(1..=8).contains(&slot) {
        return Err(ParseError::BadPatternSlot);
    }
    Ok(slot - 1)
}

fn parse_filter_command(
    parts: &mut core::str::SplitAsciiWhitespace<'_>,
) -> Result<GrooveEvent, ParseError> {
    let first = next(parts)?;
    let (band, param_token) = match first.parse::<u8>() {
        Ok(_) => (Some(parse_filter_band(first)?), next(parts)?),
        Err(_) => (None, first),
    };
    Ok(GrooveEvent::SetFilterParam {
        band,
        param: parse_filter_param(param_token)?,
        value: parse_unit(next(parts)?)?,
    })
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
        assert_eq!(parse_line("PATTERN 2"), Ok(GrooveEvent::SelectPattern(1)));
        assert_eq!(parse_line("CAPTURE 2"), Ok(GrooveEvent::CapturePattern(1)));
        assert_eq!(parse_line("BAND 3"), Ok(GrooveEvent::SelectFilterBand(2)));
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
            parse_line("BASS 4 hold"),
            Ok(GrooveEvent::SetBassStep {
                slot: None,
                step: 4,
                cell: sequencer::BassCell::Hold,
            })
        );
        assert_eq!(
            parse_line("PBASS 2 5 tie"),
            Ok(GrooveEvent::SetBassStep {
                slot: Some(1),
                step: 5,
                cell: sequencer::BassCell::Hold,
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
        assert_eq!(
            parse_line("FILTER cutoff 64"),
            Ok(GrooveEvent::SetFilterParam {
                band: None,
                param: FilterParam::Cutoff,
                value: 64.0 / 127.0,
            })
        );
        assert_eq!(
            parse_line("FILTER 4 q 32"),
            Ok(GrooveEvent::SetFilterParam {
                band: Some(3),
                param: FilterParam::Resonance,
                value: 32.0 / 127.0,
            })
        );
        assert_eq!(
            parse_line("PCOPY 1 2"),
            Ok(GrooveEvent::CopyPattern { src: 0, dst: 1 })
        );
        assert_eq!(parse_line("PCLEAR 2"), Ok(GrooveEvent::ClearPattern(1)));
        assert_eq!(
            parse_line("PFILL 1 kick 127"),
            Ok(GrooveEvent::FillPattern {
                slot: 0,
                track: Track::Kick,
                velocity: 1.0,
            })
        );
        assert_eq!(
            parse_line("PRAND 1 kick 42 64 127"),
            Ok(GrooveEvent::RandomizePattern {
                slot: 0,
                track: Track::Kick,
                seed: 42,
                density: 64.0 / 127.0,
                velocity: 1.0,
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
        assert_eq!(parse_line("BAND 9"), Err(ParseError::BadFilterBand));
        assert_eq!(parse_line("PATTERN 9"), Err(ParseError::BadPatternSlot));
        assert_eq!(parse_line("FILTER nope 1"), Err(ParseError::BadFilterParam));
        assert_eq!(parse_line("PAD 36 255"), Err(ParseError::BadNumber));
    }
}
