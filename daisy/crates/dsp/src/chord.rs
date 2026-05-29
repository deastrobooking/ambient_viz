//! Human-readable pitch parsing for the sequencer's stab lane.
//!
//! Turns text tokens into a small set of MIDI note numbers (a [`Chord`]).
//! Three token flavours, in order of how musician-friendly they are:
//!
//! 1. **Roman numerals** — diatonic to a [`Key`]: `i ii iii iv v vi vii`
//!    (case-insensitive). Built by stacking thirds *within the key's scale*,
//!    so the major/minor/diminished quality of each degree falls out
//!    automatically. A trailing `7` adds the diatonic seventh. A leading
//!    `b`/`#` borrows a chromatic root (built as a plain major/minor triad,
//!    quality taken from the numeral's case).
//! 2. **Chord names** — absolute: `C`, `Cm`, `Ab`, `Ebmaj7`, `F#m7`, `Gsus4`,
//!    `Bdim`. Root letter `A`-`G`, optional `#`/`b`, then a quality suffix.
//!    Voiced from the lane's base octave upward (close voicing).
//! 3. **Bracket stacks** — explicit voicings: `[C3 Eb3 G3]`. Each note carries
//!    its own octave (defaults to the base octave if omitted). This is the
//!    escape hatch when you want an exact voicing the namer won't give you.
//!
//! **Octave convention:** MIDI note 60 = `C4` (so `A4` = note 69 = 440 Hz,
//! standard scientific pitch notation). `C3` = 48.
//!
//! `#` and `b` are both accepted for accidentals. Whitespace is the token
//! separator at the sequencer level; this module parses one token at a time.

use heapless::Vec;

/// Maximum notes in a single stab chord.
pub const MAX_CHORD: usize = 6;

/// Default base octave for named/roman chords (where notes don't carry their
/// own octave). `3` puts a stab root around C3 (MIDI 48) — a punchy mid
/// register. Override per-pattern with the `octave:` header.
pub const DEFAULT_OCTAVE: i32 = 3;

/// A set of simultaneous MIDI notes. `Copy` so it can live in a `StepEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Chord {
    notes: [u8; MAX_CHORD],
    len: u8,
}

impl Chord {
    pub fn notes(&self) -> &[u8] {
        &self.notes[..self.len as usize]
    }

    fn push(&mut self, midi: i32) {
        if self.len as usize >= MAX_CHORD {
            return;
        }
        // Clamp into valid MIDI range rather than wrapping/overflowing.
        let m = midi.clamp(0, 127) as u8;
        self.notes[self.len as usize] = m;
        self.len += 1;
    }
}

/// A musical key: a tonic pitch class plus a 7-note scale (semitone offsets).
#[derive(Debug, Clone, Copy)]
pub struct Key {
    /// Tonic pitch class, 0 = C .. 11 = B.
    root_pc: i32,
    /// Semitone offsets of the seven scale degrees from the tonic.
    scale: [i32; 7],
}

impl Default for Key {
    /// C natural minor — the default if a pattern declares no `key:`.
    fn default() -> Self {
        Key {
            root_pc: 0,
            scale: AEOLIAN,
        }
    }
}

const IONIAN: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
const DORIAN: [i32; 7] = [0, 2, 3, 5, 7, 9, 10];
const PHRYGIAN: [i32; 7] = [0, 1, 3, 5, 7, 8, 10];
const LYDIAN: [i32; 7] = [0, 2, 4, 6, 7, 9, 11];
const MIXOLYDIAN: [i32; 7] = [0, 2, 4, 5, 7, 9, 10];
const AEOLIAN: [i32; 7] = [0, 2, 3, 5, 7, 8, 10];
const LOCRIAN: [i32; 7] = [0, 1, 3, 5, 6, 8, 10];

/// Pitch class for a note letter, or `None` if not `A`-`G`.
fn letter_pc(c: char) -> Option<i32> {
    match c {
        'C' => Some(0),
        'D' => Some(2),
        'E' => Some(4),
        'F' => Some(5),
        'G' => Some(7),
        'A' => Some(9),
        'B' => Some(11),
        _ => None,
    }
}

/// Parse a `key:` value like `C minor`, `Eb dorian`, `F# major`.
/// Returns `None` if the root note or mode can't be parsed.
pub fn parse_key(s: &str) -> Option<Key> {
    let mut it = s.split_whitespace();
    let root_tok = it.next()?;
    let mode_tok = it.next().unwrap_or("major");

    let mut chars = root_tok.chars();
    let mut root_pc = letter_pc(chars.next()?)?;
    match chars.next() {
        Some('#') => root_pc += 1,
        Some('b') => root_pc -= 1,
        _ => {}
    }
    root_pc = root_pc.rem_euclid(12);

    // Case-insensitive mode match. `min`/`maj` accepted as shorthand.
    let mode = mode_tok.to_ascii_lowercase();
    let scale = match mode.as_str() {
        "major" | "maj" | "ionian" => IONIAN,
        "minor" | "min" | "aeolian" => AEOLIAN,
        "dorian" => DORIAN,
        "phrygian" => PHRYGIAN,
        "lydian" => LYDIAN,
        "mixolydian" | "mixo" => MIXOLYDIAN,
        "locrian" => LOCRIAN,
        _ => return None,
    };

    Some(Key { root_pc, scale })
}

/// The semitone intervals (from the root) of a named chord quality.
fn quality_intervals(q: &str) -> Option<&'static [i32]> {
    Some(match q {
        "" | "maj" | "M" => &[0, 4, 7],
        "m" | "min" | "-" => &[0, 3, 7],
        "dim" | "o" => &[0, 3, 6],
        "aug" | "+" => &[0, 4, 8],
        "sus2" => &[0, 2, 7],
        "sus4" | "sus" => &[0, 5, 7],
        "7" | "dom7" => &[0, 4, 7, 10],
        "maj7" | "M7" => &[0, 4, 7, 11],
        "m7" | "min7" | "-7" => &[0, 3, 7, 10],
        "dim7" | "o7" => &[0, 3, 6, 9],
        "m7b5" | "min7b5" => &[0, 3, 6, 10],
        "6" => &[0, 4, 7, 9],
        "m6" | "min6" => &[0, 3, 7, 9],
        "add9" => &[0, 4, 7, 14],
        "9" => &[0, 4, 7, 10, 14],
        "m9" | "min9" => &[0, 3, 7, 10, 14],
        _ => return None,
    })
}

/// Parse a single stab token into a [`Chord`]. `key` and `base_octave` supply
/// context for roman numerals and named chords; bracket stacks are absolute.
/// Returns `None` for filler/rest tokens (`.`, `-`, empty) and unparseable
/// input.
pub fn parse_chord(token: &str, key: &Key, base_octave: i32) -> Option<Chord> {
    let t = token.trim();
    if t.is_empty() || t == "." || t == "-" {
        return None;
    }

    if let Some(inner) = t.strip_prefix('[') {
        let inner = inner.strip_suffix(']').unwrap_or(inner);
        return parse_bracket_stack(inner, base_octave);
    }

    let first = t.chars().next()?;
    // Roman numerals start with a numeral char, or with an accidental that is
    // immediately followed by one. Note letters are uppercase `A`-`G`, which
    // never collide with the roman chars `i I v V`.
    let is_roman = matches!(first, 'i' | 'I' | 'v' | 'V')
        || (matches!(first, 'b' | '#')
            && t.chars()
                .nth(1)
                .map(|c| matches!(c, 'i' | 'I' | 'v' | 'V'))
                .unwrap_or(false));

    if is_roman {
        parse_roman(t, key, base_octave)
    } else {
        parse_named(t, base_octave)
    }
}

/// `[C3 Eb3 G3]` body (brackets already stripped) → explicit voicing.
fn parse_bracket_stack(inner: &str, base_octave: i32) -> Option<Chord> {
    let mut chord = Chord::default();
    for tok in inner.split_whitespace() {
        let midi = parse_abs_note(tok, base_octave)?;
        chord.push(midi);
    }
    if chord.len == 0 { None } else { Some(chord) }
}

/// Parse an absolute note like `C`, `Eb`, `F#4`, `G3` into a MIDI number.
/// Octave is optional; defaults to `base_octave`.
fn parse_abs_note(tok: &str, base_octave: i32) -> Option<i32> {
    let bytes = tok.as_bytes();
    let mut pc = letter_pc(*bytes.first()? as char)?;
    let mut i = 1;
    match bytes.get(i) {
        Some(b'#') => {
            pc += 1;
            i += 1;
        }
        Some(b'b') => {
            pc -= 1;
            i += 1;
        }
        _ => {}
    }
    let octave = if i >= tok.len() {
        base_octave
    } else {
        tok[i..].parse::<i32>().ok()?
    };
    Some((octave + 1) * 12 + pc)
}

/// Parse a named chord (`Cm7`, `Ab`, `F#sus4`) voiced from `base_octave`.
fn parse_named(tok: &str, base_octave: i32) -> Option<Chord> {
    let bytes = tok.as_bytes();
    let mut pc = letter_pc(*bytes.first()? as char)?;
    // Accidental directly after the root letter.
    let mut quality_start = 1;
    match bytes.get(1) {
        Some(b'#') => {
            pc += 1;
            quality_start = 2;
        }
        Some(b'b') => {
            pc -= 1;
            quality_start = 2;
        }
        _ => {}
    }
    let quality = &tok[quality_start..];
    let intervals = quality_intervals(quality)?;

    let root_midi = (base_octave + 1) * 12 + pc;
    let mut chord = Chord::default();
    for &iv in intervals {
        chord.push(root_midi + iv);
    }
    Some(chord)
}

/// Parse a roman numeral relative to `key`, voiced from `base_octave`.
fn parse_roman(tok: &str, key: &Key, base_octave: i32) -> Option<Chord> {
    let mut s = tok;
    let mut accidental = 0;
    if let Some(rest) = s.strip_prefix('b') {
        accidental = -1;
        s = rest;
    } else if let Some(rest) = s.strip_prefix('#') {
        accidental = 1;
        s = rest;
    }

    // Leading run of roman chars is the degree; the remainder is a suffix
    // (currently only `7` is honoured).
    let numeral_len = s
        .find(|c: char| !matches!(c, 'i' | 'I' | 'v' | 'V'))
        .unwrap_or(s.len());
    let numeral = &s[..numeral_len];
    let suffix = &s[numeral_len..];
    let seventh = suffix.contains('7');

    let degree = roman_degree(numeral)?; // 1..=7
    let uppercase = numeral.chars().next()?.is_ascii_uppercase();

    let mut chord = Chord::default();
    let octave_base = (base_octave + 1) * 12 + key.root_pc;

    if accidental != 0 {
        // Borrowed/chromatic root: build a plain triad, quality from case.
        let root = octave_base + scale_value(&key.scale, degree) + accidental;
        let intervals: &[i32] = if uppercase {
            if seventh { &[0, 4, 7, 10] } else { &[0, 4, 7] }
        } else if seventh {
            &[0, 3, 7, 10]
        } else {
            &[0, 3, 7]
        };
        for &iv in intervals {
            chord.push(root + iv);
        }
    } else {
        // Diatonic: stack thirds within the scale (third, fifth, [seventh]).
        chord.push(octave_base + scale_value(&key.scale, degree));
        chord.push(octave_base + scale_value(&key.scale, degree + 2));
        chord.push(octave_base + scale_value(&key.scale, degree + 4));
        if seventh {
            chord.push(octave_base + scale_value(&key.scale, degree + 6));
        }
    }
    Some(chord)
}

/// Roman numeral string → 1-based scale degree.
fn roman_degree(s: &str) -> Option<i32> {
    match s.to_ascii_lowercase().as_str() {
        "i" => Some(1),
        "ii" => Some(2),
        "iii" => Some(3),
        "iv" => Some(4),
        "v" => Some(5),
        "vi" => Some(6),
        "vii" => Some(7),
        _ => None,
    }
}

/// Semitone offset of a (possibly >7) scale degree, wrapping octaves upward so
/// that stacked thirds ascend. `degree` is 1-based.
fn scale_value(scale: &[i32; 7], degree: i32) -> i32 {
    let idx = degree - 1;
    scale[(idx.rem_euclid(7)) as usize] + 12 * idx.div_euclid(7)
}

/// Tokenize a `prog:` line into individual chord tokens, keeping `[...]`
/// bracket stacks intact. Whitespace, `|` and `,` separate tokens; a
/// standalone `.` or `-` is visual filler and is dropped.
pub fn tokenize_prog<const N: usize>(line: &str) -> Vec<heapless::String<24>, N> {
    let mut out: Vec<heapless::String<24>, N> = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'|' | b',' | b'.' | b'-' => {
                i += 1;
            }
            b'[' => {
                let start = i;
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1; // include the closing ']'
                }
                push_tok(&mut out, &line[start..i]);
            }
            _ => {
                let start = i;
                while i < bytes.len() && !matches!(bytes[i], b' ' | b'\t' | b'|' | b',') {
                    i += 1;
                }
                push_tok(&mut out, &line[start..i]);
            }
        }
    }
    out
}

fn push_tok<const N: usize>(out: &mut Vec<heapless::String<24>, N>, s: &str) {
    if s.is_empty() {
        return;
    }
    let mut t: heapless::String<24> = heapless::String::new();
    if t.push_str(s).is_ok() {
        let _ = out.push(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_cmin() -> Key {
        parse_key("C minor").unwrap()
    }

    #[test]
    fn named_minor_triad() {
        // Cm at octave 4 → C4(60), Eb4(63), G4(67).
        let c = parse_named("Cm", 4).unwrap();
        assert_eq!(c.notes(), &[60, 63, 67]);
    }

    #[test]
    fn named_with_accidental_and_seventh() {
        // Ebmaj7 at octave 3 → Eb3(51), G3(55), Bb3(58), D4(62).
        let c = parse_named("Ebmaj7", 3).unwrap();
        assert_eq!(c.notes(), &[51, 55, 58, 62]);
    }

    #[test]
    fn roman_i_in_c_minor() {
        // i in C minor = C Eb G.
        let c = parse_roman("i", &key_cmin(), 4).unwrap();
        assert_eq!(c.notes(), &[60, 63, 67]);
    }

    #[test]
    fn roman_iv_in_c_minor() {
        // iv in C minor = F Ab C.
        let c = parse_roman("iv", &key_cmin(), 4).unwrap();
        assert_eq!(c.notes(), &[65, 68, 72]);
    }

    #[test]
    fn roman_v7_in_c_minor_is_diatonic_minor_seventh() {
        // v7 diatonic in aeolian = G Bb D F.
        let c = parse_roman("v7", &key_cmin(), 4).unwrap();
        assert_eq!(c.notes(), &[67, 70, 74, 77]);
    }

    #[test]
    fn bracket_stack_explicit_voicing() {
        let c = parse_chord("[C3 Eb3 G3 Bb3]", &key_cmin(), 4).unwrap();
        assert_eq!(c.notes(), &[48, 51, 55, 58]);
    }

    #[test]
    fn dispatch_picks_roman_vs_named() {
        // "V" is roman (degree 5), "G" is the absolute note G.
        let roman = parse_chord("V", &key_cmin(), 4).unwrap();
        let named = parse_chord("G", &key_cmin(), 4).unwrap();
        assert_eq!(roman.notes()[0], 67); // G as the root of v
        assert_eq!(named.notes(), &[67, 71, 74]); // G B D (major)
    }

    #[test]
    fn rests_and_filler_are_none() {
        assert!(parse_chord(".", &key_cmin(), 4).is_none());
        assert!(parse_chord("-", &key_cmin(), 4).is_none());
        assert!(parse_chord("", &key_cmin(), 4).is_none());
    }

    #[test]
    fn tokenize_keeps_brackets_together() {
        let toks: Vec<_, 16> = tokenize_prog("Cm . Ab [C3 Eb3 G3] | Eb");
        let strs: alloc::vec::Vec<&str> = toks.iter().map(|s| s.as_str()).collect();
        assert_eq!(strs, ["Cm", "Ab", "[C3 Eb3 G3]", "Eb"]);
    }
}
