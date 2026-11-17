//! Raw MIDI message parsing / encoding. Channels are 1-based (1..=16) everywhere
//! in this crate, matching the legacy mapping files and open-stage-control.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiMsg {
    /// Note on or off. A note-off (0x8n) is reported as velocity 0.
    Note { ch: u8, note: u8, vel: u8 },
    Cc { ch: u8, cc: u8, val: u8 },
    /// 14-bit pitch bend, 0..=16383.
    Pitch { ch: u8, val: u16 },
    SysEx(Vec<u8>),
}

impl MidiMsg {
    pub fn parse(bytes: &[u8]) -> Option<MidiMsg> {
        let status = *bytes.first()?;
        let ch = (status & 0x0F) + 1;
        let d1 = bytes.get(1).copied().unwrap_or(0) & 0x7F;
        let d2 = bytes.get(2).copied().unwrap_or(0) & 0x7F;
        match status & 0xF0 {
            0x80 if bytes.len() >= 3 => Some(MidiMsg::Note { ch, note: d1, vel: 0 }),
            0x90 if bytes.len() >= 3 => Some(MidiMsg::Note { ch, note: d1, vel: d2 }),
            0xB0 if bytes.len() >= 3 => Some(MidiMsg::Cc { ch, cc: d1, val: d2 }),
            0xE0 if bytes.len() >= 3 => Some(MidiMsg::Pitch { ch, val: (d1 as u16) | ((d2 as u16) << 7) }),
            0xF0 if status == 0xF0 => Some(MidiMsg::SysEx(bytes.to_vec())),
            _ => None,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let c = |ch: u8| (ch.clamp(1, 16) - 1) & 0x0F;
        match self {
            MidiMsg::Note { ch, note, vel } => vec![0x90 | c(*ch), note & 0x7F, vel & 0x7F],
            MidiMsg::Cc { ch, cc, val } => vec![0xB0 | c(*ch), cc & 0x7F, val & 0x7F],
            MidiMsg::Pitch { ch, val } => {
                let v = (*val).min(16383);
                vec![0xE0 | c(*ch), (v & 0x7F) as u8, (v >> 7) as u8]
            }
            MidiMsg::SysEx(b) => b.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for m in [
            MidiMsg::Note { ch: 1, note: 104, vel: 127 },
            MidiMsg::Cc { ch: 16, cc: 48, val: 33 },
            MidiMsg::Pitch { ch: 9, val: 16383 },
            MidiMsg::Pitch { ch: 1, val: 0 },
        ] {
            assert_eq!(MidiMsg::parse(&m.encode()), Some(m));
        }
    }

    #[test]
    fn note_off_is_velocity_zero() {
        assert_eq!(MidiMsg::parse(&[0x80, 10, 64]), Some(MidiMsg::Note { ch: 1, note: 10, vel: 0 }));
    }

    #[test]
    fn garbage_is_ignored() {
        assert_eq!(MidiMsg::parse(&[]), None);
        assert_eq!(MidiMsg::parse(&[0x90, 1]), None);
        assert_eq!(MidiMsg::parse(&[0xF8]), None);
    }
}
