//! Desired-state model of one control surface.
//!
//! Every LED, motor fader, LED ring, LCD field and 7-segment digit is a key
//! with a desired value. Output is "send whatever differs from what the
//! device last received". This gives:
//!  * full resync after a reconnect / power cycle (clear `sent`, send all),
//!  * no redundant MIDI traffic,
//!  * fader holds: while a fader is touched (or was just moved) feedback for
//!    it is recorded but not sent, so the motor never fights the operator.

use crate::midi::MidiMsg;
use crate::xtouch;
use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OutKey {
    Note { ch: u8, note: u8 },
    Cc { ch: u8, cc: u8 },
    Pitch { ch: u8 },
    LcdText { offset: u8 },
    LcdColors,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutVal {
    U7(u8),
    U14(u16),
    Text([u8; xtouch::STRIP_CHARS]),
    Colors([u8; 8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    Touch,
    Until(Instant),
}

#[derive(Debug)]
pub struct Surface {
    sysex_id: u8,
    connected: bool,
    desired: BTreeMap<OutKey, OutVal>,
    sent: HashMap<OutKey, OutVal>,
    holds: HashMap<OutKey, Hold>,
    /// Fader/CC values sent since the last `take_moves`: (key, previous, new).
    moves: Vec<(OutKey, Option<OutVal>, OutVal)>,
}

impl Surface {
    pub fn new(sysex_id: u8) -> Self {
        Surface { sysex_id, connected: false, desired: BTreeMap::new(), sent: HashMap::new(), holds: HashMap::new(), moves: Vec::new() }
    }

    /// Set a desired value; queue MIDI if it differs from what the device has.
    pub fn set(&mut self, key: OutKey, val: OutVal, out: &mut Vec<Vec<u8>>) {
        self.desired.insert(key, val);
        self.flush_key(key, out);
    }

    /// The operator physically moved this control to `val`: the device already
    /// shows it, so record it as sent and hold feedback until `until`.
    pub fn local_change(&mut self, key: OutKey, val: OutVal, until: Instant) {
        self.desired.insert(key, val.clone());
        if self.connected {
            self.sent.insert(key, val);
        }
        self.hold_until(key, until);
    }

    pub fn hold_touch(&mut self, key: OutKey) {
        self.holds.insert(key, Hold::Touch);
    }

    pub fn hold_until(&mut self, key: OutKey, until: Instant) {
        match self.holds.get(&key) {
            Some(Hold::Touch) => {}
            Some(Hold::Until(t)) if *t >= until => {}
            _ => {
                self.holds.insert(key, Hold::Until(until));
            }
        }
    }

    /// Touch released: keep holding for a short echo-guard period, then resume.
    pub fn release_touch(&mut self, key: OutKey, until: Instant) {
        self.holds.insert(key, Hold::Until(until));
    }

    /// Release expired holds and send whatever changed meanwhile.
    pub fn expire_holds(&mut self, now: Instant, out: &mut Vec<Vec<u8>>) {
        let expired: Vec<OutKey> =
            self.holds.iter().filter(|(_, h)| matches!(h, Hold::Until(t) if *t <= now)).map(|(k, _)| *k).collect();
        for k in expired {
            self.holds.remove(&k);
            self.flush_key(k, out);
        }
    }

    /// Fader positions (pitch and CC) sent to the device since the last call,
    /// as (key, value the device had before, new value).
    pub fn take_moves(&mut self) -> Vec<(OutKey, Option<OutVal>, OutVal)> {
        std::mem::take(&mut self.moves)
    }

    pub fn connect(&mut self, out: &mut Vec<Vec<u8>>) {
        self.connected = true;
        self.resync(out);
    }

    pub fn disconnect(&mut self) {
        self.connected = false;
        self.sent.clear();
        self.holds.clear();
        self.moves.clear();
    }

    /// Forget what the device has and send the complete desired state.
    pub fn resync(&mut self, out: &mut Vec<Vec<u8>>) {
        self.sent.clear();
        let keys: Vec<OutKey> = self.desired.keys().copied().collect();
        for k in keys {
            self.flush_key(k, out);
        }
    }

    fn flush_key(&mut self, key: OutKey, out: &mut Vec<Vec<u8>>) {
        if !self.connected || self.holds.contains_key(&key) {
            return;
        }
        let Some(val) = self.desired.get(&key) else { return };
        if self.sent.get(&key) == Some(val) {
            return;
        }
        if let Some(bytes) = encode(self.sysex_id, key, val) {
            out.push(bytes);
            let prev = self.sent.insert(key, val.clone());
            if matches!(key, OutKey::Cc { .. } | OutKey::Pitch { .. }) {
                self.moves.push((key, prev, val.clone()));
            }
        }
    }
}

fn encode(sysex_id: u8, key: OutKey, val: &OutVal) -> Option<Vec<u8>> {
    Some(match (key, val) {
        (OutKey::Note { ch, note }, OutVal::U7(v)) => MidiMsg::Note { ch, note, vel: *v }.encode(),
        (OutKey::Cc { ch, cc }, OutVal::U7(v)) => MidiMsg::Cc { ch, cc, val: *v }.encode(),
        (OutKey::Pitch { ch }, OutVal::U14(v)) => MidiMsg::Pitch { ch, val: *v }.encode(),
        (OutKey::LcdText { offset }, OutVal::Text(t)) => xtouch::lcd_text_sysex(sysex_id, offset, t),
        (OutKey::LcdColors, OutVal::Colors(c)) => xtouch::lcd_colors_sysex(sysex_id, c),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const K: OutKey = OutKey::Pitch { ch: 1 };

    fn connected() -> Surface {
        let mut s = Surface::new(0x14);
        s.connect(&mut vec![]);
        s
    }

    #[test]
    fn diff_only() {
        let mut s = connected();
        let mut out = vec![];
        s.set(K, OutVal::U14(100), &mut out);
        s.set(K, OutVal::U14(100), &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn resync_after_reconnect() {
        let mut s = connected();
        let mut out = vec![];
        s.set(K, OutVal::U14(100), &mut out);
        s.set(OutKey::Note { ch: 1, note: 5 }, OutVal::U7(127), &mut out);
        s.disconnect();
        s.set(K, OutVal::U14(200), &mut out); // changes while unplugged are kept
        out.clear();
        s.connect(&mut out);
        assert_eq!(out.len(), 2);
        assert!(out.contains(&MidiMsg::Pitch { ch: 1, val: 200 }.encode()));
    }

    #[test]
    fn touch_hold_defers_feedback() {
        let mut s = connected();
        let t0 = Instant::now();
        let mut out = vec![];
        s.hold_touch(K);
        s.set(K, OutVal::U14(500), &mut out);
        s.expire_holds(t0 + Duration::from_secs(10), &mut out);
        assert!(out.is_empty(), "touched fader must not move");
        s.release_touch(K, t0 + Duration::from_millis(300));
        s.expire_holds(t0 + Duration::from_millis(100), &mut out);
        assert!(out.is_empty());
        s.expire_holds(t0 + Duration::from_millis(300), &mut out);
        assert_eq!(out, vec![MidiMsg::Pitch { ch: 1, val: 500 }.encode()]);
    }

    #[test]
    fn local_change_swallows_matching_echo() {
        let mut s = connected();
        let t0 = Instant::now();
        let mut out = vec![];
        s.local_change(K, OutVal::U14(800), t0 + Duration::from_millis(300));
        s.set(K, OutVal::U14(700), &mut out); // stale echo from MA
        s.set(K, OutVal::U14(800), &mut out); // final echo matches the hand
        s.expire_holds(t0 + Duration::from_millis(300), &mut out);
        assert!(out.is_empty());
    }
}
