//! MIDI parsing. Same decoder runs on Mac CoreMIDI bytes and Daisy UART bytes.
//!
//! Channel-voice messages only — system real-time / SysEx are dropped.
//! Running status is not handled here; both midir (host) and the Daisy UART
//! parser are expected to emit complete status+data byte sequences per call.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiMessage {
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8, velocity: u8 },
    ControlChange { channel: u8, cc: u8, value: u8 },
    PitchBend { channel: u8, value: i16 },
}

/// Decode a single channel-voice message. Returns `None` for incomplete
/// buffers, non-status leading bytes, or unrecognized message types.
pub fn decode(bytes: &[u8]) -> Option<MidiMessage> {
    if bytes.len() < 2 {
        return None;
    }
    let status = bytes[0];
    if status < 0x80 {
        return None;
    }
    let channel = status & 0x0F;
    match status & 0xF0 {
        0x80 if bytes.len() >= 3 => Some(MidiMessage::NoteOff {
            channel,
            note: bytes[1],
            velocity: bytes[2],
        }),
        0x90 if bytes.len() >= 3 => {
            // Note-on with velocity 0 is conventionally treated as note-off.
            if bytes[2] == 0 {
                Some(MidiMessage::NoteOff {
                    channel,
                    note: bytes[1],
                    velocity: 0,
                })
            } else {
                Some(MidiMessage::NoteOn {
                    channel,
                    note: bytes[1],
                    velocity: bytes[2],
                })
            }
        }
        0xB0 if bytes.len() >= 3 => Some(MidiMessage::ControlChange {
            channel,
            cc: bytes[1],
            value: bytes[2],
        }),
        0xE0 if bytes.len() >= 3 => {
            // 14-bit value centered at 8192.
            let value = (((bytes[2] as i16) << 7) | bytes[1] as i16) - 8192;
            Some(MidiMessage::PitchBend { channel, value })
        }
        _ => None,
    }
}
