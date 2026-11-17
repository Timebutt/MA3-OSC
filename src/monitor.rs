//! Message monitor: a capture of MIDI and OSC traffic for the monitor window.
//!
//! Capturing is OFF unless the monitor window is open; when off, every capture
//! call is a single atomic load, so the monitor costs nothing during a show.

use crate::midi::MidiMsg;
use crate::xtouch;
use rosc::OscType;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Entries kept in memory (oldest are dropped first).
const CAPACITY: usize = 5000;
/// Bytes of a message shown as hex.
const HEX_MAX: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Dir {
    #[serde(rename = "midi-in")]
    MidiIn,
    #[serde(rename = "midi-out")]
    MidiOut,
    #[serde(rename = "osc-in")]
    OscIn,
    #[serde(rename = "osc-out")]
    OscOut,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub seq: u64,
    /// Milliseconds since 1970 (the window shows it in local time).
    pub ms: u64,
    pub dir: Dir,
    /// Device name from the config, or "MA" for OSC.
    pub dev: String,
    /// Human-readable meaning.
    pub text: String,
    /// Raw bytes (MIDI only).
    pub hex: String,
}

struct Buffer {
    entries: VecDeque<Entry>,
    next_seq: u64,
}

pub struct Monitor {
    enabled: AtomicBool,
    buf: Mutex<Buffer>,
}

pub fn get() -> &'static Monitor {
    static M: OnceLock<Monitor> = OnceLock::new();
    M.get_or_init(|| Monitor {
        enabled: AtomicBool::new(false),
        buf: Mutex::new(Buffer { entries: VecDeque::with_capacity(CAPACITY), next_seq: 1 }),
    })
}

impl Monitor {
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    #[inline]
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn midi(&self, dir: Dir, dev: &str, bytes: &[u8]) {
        if self.enabled() {
            self.push(dir, dev, describe_midi(bytes), hex(bytes));
        }
    }

    #[inline]
    pub fn osc(&self, dir: Dir, addr: &str, args: &[OscType]) {
        if self.enabled() {
            self.push(dir, "MA", describe_osc(addr, args), String::new());
        }
    }

    fn push(&self, dir: Dir, dev: &str, text: String, hex: String) {
        let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let mut b = self.buf.lock().unwrap_or_else(|p| p.into_inner());
        let seq = b.next_seq;
        b.next_seq += 1;
        if b.entries.len() >= CAPACITY {
            b.entries.pop_front();
        }
        b.entries.push_back(Entry { seq, ms, dir, dev: dev.to_string(), text, hex });
    }

    /// Entries with seq > `after` (at most `max`), and how many were already dropped.
    pub fn since(&self, after: u64, max: usize) -> (Vec<Entry>, u64) {
        let b = self.buf.lock().unwrap_or_else(|p| p.into_inner());
        let first = b.entries.front().map(|e| e.seq).unwrap_or(b.next_seq);
        let dropped = first.saturating_sub(after + 1);
        let out = b.entries.iter().filter(|e| e.seq > after).take(max).cloned().collect();
        (out, dropped)
    }
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s: Vec<String> = bytes.iter().take(HEX_MAX).map(|b| format!("{b:02X}")).collect();
    if bytes.len() > HEX_MAX {
        s.push(format!("… ({} bytes)", bytes.len()));
    }
    s.join(" ")
}

pub fn describe_midi(bytes: &[u8]) -> String {
    match MidiMsg::parse(bytes) {
        Some(MidiMsg::Note { ch, note, vel }) => {
            let state = if vel == 0 { "off".to_string() } else { format!("vel {vel}") };
            format!("Note   ch {ch:<2} note {note:<3} {state}")
        }
        Some(MidiMsg::Cc { ch, cc, val }) => format!("CC     ch {ch:<2} cc   {cc:<3} val {val}"),
        Some(MidiMsg::Pitch { ch, val }) => {
            format!("Fader  ch {ch:<2} {val:>5}  ({}%)", (val as u32 * 100 + 8191) / 16383)
        }
        Some(MidiMsg::SysEx(b)) => describe_sysex(&b),
        None => "unknown message".into(),
    }
}

fn describe_sysex(b: &[u8]) -> String {
    let unit = |id: u8| match id {
        xtouch::MC_DEVICE_ID => "X-Touch",
        xtouch::MC_EXTENDER_ID => "Extender",
        _ => "Mackie",
    };
    if b.len() >= 7 && b[1..4] == [0x00, 0x00, 0x66] {
        let (id, cmd) = (b[4], b[5]);
        if cmd == 0x12 && b.len() >= 8 {
            let offset = b[6] as usize;
            let text: String = b[7..b.len() - 1].iter().map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { '?' }).collect();
            let (row, strip) = (offset / 56, (offset % 56) / xtouch::STRIP_CHARS);
            return format!("LCD    {} strip {} {} \"{text}\"", unit(id), strip + 1, if row == 0 { "top" } else { "bottom" });
        }
        if cmd == 0x72 {
            const NAMES: [&str; 8] = ["off", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
            let colors: Vec<&str> = b[6..b.len() - 1].iter().map(|&c| NAMES.get(c as usize).copied().unwrap_or("?")).collect();
            return format!("Colors {} {}", unit(id), colors.join(" "));
        }
        return format!("SysEx  {} command {cmd:02X}", unit(id));
    }
    format!("SysEx  {} bytes", b.len())
}

pub fn describe_osc(addr: &str, args: &[OscType]) -> String {
    let args: Vec<String> = args
        .iter()
        .map(|a| match a {
            OscType::Int(i) => i.to_string(),
            OscType::Float(f) => format!("{f}"),
            OscType::Double(f) => format!("{f}"),
            OscType::Long(i) => i.to_string(),
            OscType::String(s) => format!("\"{s}\""),
            OscType::Bool(b) => if *b { "T" } else { "F" }.to_string(),
            OscType::Nil => "nil".into(),
            other => format!("{other:?}"),
        })
        .collect();
    if args.is_empty() { addr.to_string() } else { format!("{addr}  {}", args.join(" ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_messages() {
        assert_eq!(describe_midi(&[0x90, 104, 127]), "Note   ch 1  note 104 vel 127");
        assert_eq!(describe_midi(&[0x80, 104, 64]), "Note   ch 1  note 104 off");
        assert_eq!(describe_midi(&[0xB0, 16, 65]), "CC     ch 1  cc   16  val 65");
        assert_eq!(describe_midi(&[0xE1, 0x7F, 0x7F]), "Fader  ch 2  16383  (100%)");
        let lcd = xtouch::lcd_text_sysex(0x15, 63, b"Cue 3  ");
        assert_eq!(describe_midi(&lcd), "LCD    Extender strip 2 bottom \"Cue 3  \"");
        let col = xtouch::lcd_colors_sysex(0x14, &[1, 0, 7, 0, 0, 0, 0, 2]);
        assert_eq!(describe_midi(&col), "Colors X-Touch red off white off off off off green");
        assert_eq!(
            describe_osc("/Page1/Fader201", &[OscType::Int(127), OscType::String("x".into())]),
            "/Page1/Fader201  127 \"x\""
        );
        assert_eq!(hex(&[0x90, 1, 2]), "90 01 02");
    }

    #[test]
    fn capture_only_when_enabled_and_bounded() {
        let m = Monitor {
            enabled: AtomicBool::new(false),
            buf: Mutex::new(Buffer { entries: VecDeque::new(), next_seq: 1 }),
        };
        m.midi(Dir::MidiIn, "x", &[0x90, 1, 1]);
        assert!(m.since(0, 10).0.is_empty());
        m.set_enabled(true);
        for _ in 0..CAPACITY + 10 {
            m.osc(Dir::OscOut, "/a", &[]);
        }
        let (all, dropped) = m.since(0, usize::MAX);
        assert_eq!(all.len(), CAPACITY);
        assert_eq!(dropped, 10);
        let (tail, dropped) = m.since(all[all.len() - 3].seq, 100);
        assert_eq!((tail.len(), dropped), (2, 0));
    }
}
