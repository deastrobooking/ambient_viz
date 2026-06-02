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

/// Data bytes expected after a channel-voice status byte.
fn expected_data(status: u8) -> usize {
    match status & 0xF0 {
        0xC0 | 0xD0 => 1, // program change / channel pressure
        _ => 2,           // note on/off, CC, pitch bend
    }
}

/// Streaming byte accumulator: turns an unframed MIDI byte stream (CDC OUT or
/// TRS-UART) into complete [`MidiMessage`]s for [`decode`]. MIDI is self-framing
/// — status bytes have bit 7 set — so we buffer from a status byte until its
/// data bytes arrive. Supports running status; ignores System Real-Time bytes
/// (>= 0xF8, which may interleave) and drops System Common (0xF0-0xF7).
pub struct MidiByteParser {
    status: u8, // current running status (0 = none yet)
    data: [u8; 2],
    data_len: usize,
    expected: usize,
}

impl Default for MidiByteParser {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiByteParser {
    pub const fn new() -> Self {
        Self {
            status: 0,
            data: [0; 2],
            data_len: 0,
            expected: 0,
        }
    }

    /// Feed one byte. Returns a message once a complete frame is assembled.
    pub fn push(&mut self, byte: u8) -> Option<MidiMessage> {
        if byte >= 0xF8 {
            // System Real-Time — does not disturb the running message.
            return None;
        }
        if byte >= 0x80 {
            if byte >= 0xF0 {
                // System Common — unsupported; clear state.
                self.status = 0;
                self.data_len = 0;
                return None;
            }
            // New channel-voice status byte.
            self.status = byte;
            self.data_len = 0;
            self.expected = expected_data(byte);
            return None;
        }
        // Data byte.
        if self.status == 0 {
            return None; // no status seen yet
        }
        if self.data_len < self.data.len() {
            self.data[self.data_len] = byte;
            self.data_len += 1;
        }
        if self.data_len >= self.expected {
            let frame = [self.status, self.data[0], self.data[1]];
            let msg = decode(&frame[..1 + self.expected]);
            self.data_len = 0; // keep status for running status
            return msg;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(p: &mut MidiByteParser, bytes: &[u8]) -> alloc::vec::Vec<MidiMessage> {
        bytes.iter().filter_map(|&b| p.push(b)).collect()
    }

    #[test]
    fn assembles_cc_from_bytes() {
        let mut p = MidiByteParser::new();
        assert_eq!(
            feed(&mut p, &[0xB0, 23, 127]),
            [MidiMessage::ControlChange {
                channel: 0,
                cc: 23,
                value: 127
            }]
        );
    }

    #[test]
    fn running_status_reuses_last_status() {
        let mut p = MidiByteParser::new();
        assert_eq!(
            feed(&mut p, &[0xB1, 23, 64, 24, 100]),
            [
                MidiMessage::ControlChange {
                    channel: 1,
                    cc: 23,
                    value: 64
                },
                MidiMessage::ControlChange {
                    channel: 1,
                    cc: 24,
                    value: 100
                },
            ]
        );
    }

    #[test]
    fn ignores_realtime_mid_message() {
        let mut p = MidiByteParser::new();
        assert_eq!(
            feed(&mut p, &[0xB0, 23, 0xF8, 90]),
            [MidiMessage::ControlChange {
                channel: 0,
                cc: 23,
                value: 90
            }]
        );
    }

    #[test]
    fn leading_data_bytes_ignored() {
        let mut p = MidiByteParser::new();
        assert!(feed(&mut p, &[10, 20, 30]).is_empty());
    }
}
