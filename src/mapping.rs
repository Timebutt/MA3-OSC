//! Device mapping model + loader for the legacy pam-OSC mapping JSON format
//! (the `mappings/*.json` files used by the open-stage-control script).
//!
//! Loading is strict: unknown keys, wrong types and out-of-range values are
//! reported with the file name and the exact location, and the file is rejected.
//! The JavaScript `buttonFeedbackMapper` functions are NOT evaluated; they are
//! recognised as simple On/Off lookup tables (the only form used in practice).

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub type Exec = u16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feedback {
    pub on: u8,
    pub off: u8,
}

impl Default for Feedback {
    fn default() -> Self {
        Feedback { on: 127, off: 0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Local {
    EncoderFine,
    EncoderRough,
    Attribute(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NoteAction {
    pub channel: Option<u8>,
    /// Ignore messages with velocity <= min_value (used to fire only on press).
    pub min_value: Option<u8>,
    pub exec: Option<Exec>,
    pub cmd: Option<String>,
    pub quickey: Option<String>,
    pub local: Option<Local>,
    /// Attribute name for attribute-select LEDs (legacy `attribute` key).
    pub attribute: Option<String>,
    pub timecode_select: bool,
    pub timecode_play_pause: bool,
    /// Fixed LED velocity that overrides feedback from MA.
    pub permanent_feedback: Option<u8>,
    pub feedback: Option<Feedback>,
    /// Always send the executor key, even when MA's command line waits for a destination.
    pub ignore_cmdline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RelRange {
    pub pos_from: u8,
    pub pos_to: u8,
    pub neg_from: u8,
    pub neg_to: u8,
}

impl RelRange {
    /// Decode a relative encoder value into a signed step, or None if the
    /// value is outside both ranges (legacy code turned that into NaN -> 0).
    pub fn step(&self, v: u8) -> Option<i32> {
        if v >= self.pos_from && v <= self.pos_to {
            Some(v as i32 - self.pos_from as i32 + 1)
        } else if v >= self.neg_from && v <= self.neg_to {
            Some(-(v as i32 - self.neg_from as i32 + 1))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LedRing {
    pub cc: u8,
    pub from: u8,
    pub to: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelExec {
    pub exec: Exec,
    pub initial: u8,
    pub ring: Option<LedRing>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelAttribute {
    /// Attribute name, or "current" for the currently selected attribute.
    pub attribute: String,
    pub amount: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Relative {
    pub range: RelRange,
    pub exec: Option<RelExec>,
    pub attribute: Option<RelAttribute>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceMapping {
    /// Mackie Control mode (full X-Touch): note-off via velocity 0 on ch 1,
    /// fader touch on notes 104..=112, LCD scribble strips, 7-segment display.
    pub mc_mode: bool,
    /// X-Touch Extender (Mackie Control, sysex device ID 0x15, no timecode display).
    pub extender: bool,
    pub timecode_display: bool,
    pub feedback: Feedback,
    pub default_channel: u8,
    /// CC number -> fader executor (input on any channel, feedback on ch 1).
    pub control: BTreeMap<u8, Exec>,
    /// Pitch-bend channel (1-based) -> fader executor.
    pub pitch: BTreeMap<u8, Exec>,
    pub relative: BTreeMap<u8, Relative>,
    pub notes: BTreeMap<u8, NoteAction>,
    /// LCD strip index (0..=7) -> executor whose name/color it shows.
    pub display: BTreeMap<u8, Exec>,
    /// Touch-sense note -> fader executor.
    pub fader_touch: BTreeMap<u8, Exec>,
}

pub fn load_file(path: &Path) -> Result<DeviceMapping> {
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read mapping {}", path.display()))?;
    let is_v2 = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("format").cloned())
        .is_some();
    let parsed = if is_v2 { crate::mapping_v2::parse_v2(&text) } else { parse_legacy(&text) };
    parsed.with_context(|| format!("invalid mapping {}", path.display()))
}

pub fn parse_legacy(text: &str) -> Result<DeviceMapping> {
    let root: Value = serde_json::from_str(text).context("not valid JSON")?;
    let obj = root.as_object().ok_or_else(|| anyhow!("top level must be an object"))?;

    let mut m = DeviceMapping { default_channel: 1, ..Default::default() };

    for (key, val) in obj {
        let at = key.as_str();
        match at {
            "mode" => {
                let mode = as_str(val, at)?;
                (m.mc_mode, m.extender) = match mode {
                    "mc" => (true, false),
                    "mc-extender" => (true, true),
                    "" => (false, false),
                    other => bail!("{at}: unknown mode '{other}' (use \"mc\" or \"mc-extender\")"),
                };
            }
            "enableTimecodeSend" => m.timecode_display = as_bool(val, at)?,
            "buttonFeedbackMapper" => m.feedback = parse_feedback_fn(as_str(val, at)?).context(at.to_string())?,
            "midiChannel" => m.default_channel = as_channel(val, at)?,
            "control" => {
                for (k, v) in as_obj(val, at)? {
                    let at = format!("control.{k}");
                    m.control.insert(as_u7_key(k, &at)?, as_exec(v, &at)?);
                }
            }
            "pitch" => {
                for (k, v) in as_obj(val, at)? {
                    let at = format!("pitch.{k}");
                    let ch = k.parse::<u8>().ok().filter(|c| (1..=16).contains(c));
                    let ch = ch.ok_or_else(|| anyhow!("{at}: key must be a MIDI channel 1-16"))?;
                    m.pitch.insert(ch, as_exec(v, &at)?);
                }
            }
            "display" => {
                for (k, v) in as_obj(val, at)? {
                    let at = format!("display.{k}");
                    let strip = k.parse::<u8>().ok().filter(|s| *s < 8);
                    let strip = strip.ok_or_else(|| anyhow!("{at}: key must be an LCD strip 0-7"))?;
                    m.display.insert(strip, as_exec(v, &at)?);
                }
            }
            "faderTouch" => {
                for (k, v) in as_obj(val, at)? {
                    let at = format!("faderTouch.{k}");
                    m.fader_touch.insert(as_u7_key(k, &at)?, as_exec(v, &at)?);
                }
            }
            "rltvControl" => {
                for (k, v) in as_obj(val, at)? {
                    let at = format!("rltvControl.{k}");
                    m.relative.insert(as_u7_key(k, &at)?, parse_relative(v, &at)?);
                }
            }
            "note" => {
                for (k, v) in as_obj(val, at)? {
                    let at = format!("note.{k}");
                    m.notes.insert(as_u7_key(k, &at)?, parse_note(v, &at)?);
                }
            }
            other => bail!("unknown key '{other}'"),
        }
    }

    if m.extender && m.timecode_display {
        bail!("enableTimecodeSend: the X-Touch Extender has no timecode display");
    }
    add_mackie_touch(&mut m);
    Ok(m)
}

/// Mackie Control: fader touch notes 104..=112 belong to pitch channels 1..=9,
/// unless the mapping explicitly uses those notes for something else.
pub fn add_mackie_touch(m: &mut DeviceMapping) {
    if !m.mc_mode {
        return;
    }
    for (ch, exec) in &m.pitch {
        let note = 103 + ch;
        if *ch <= 9 && !m.notes.contains_key(&note) {
            m.fader_touch.entry(note).or_insert(*exec);
        }
    }
}

fn parse_note(v: &Value, at: &str) -> Result<NoteAction> {
    let mut n = NoteAction::default();
    let mut local: Option<String> = None;
    let note_at = at;
    for (key, val) in as_obj(v, at)? {
        let at = format!("{at}.{key}");
        match key.as_str() {
            "exec" => n.exec = Some(as_exec(val, &at)?),
            "cmd" => n.cmd = Some(as_str(val, &at)?.to_string()),
            "quicKey" => n.quickey = Some(as_str(val, &at)?.to_string()),
            "local" => local = Some(as_str(val, &at)?.to_string()),
            "attribute" => n.attribute = Some(as_str(val, &at)?.to_string()),
            "minValue" => n.min_value = Some(as_u7(val, &at)?).filter(|v| *v > 0),
            "midiChannel" => n.channel = Some(as_channel(val, &at)?),
            "permanentFeedback" => n.permanent_feedback = Some(as_u7(val, &at)?).filter(|v| *v > 0),
            "timecodeSelect" => n.timecode_select = as_bool(val, &at)?,
            "timecodePlayPause" => n.timecode_play_pause = as_bool(val, &at)?,
            "ignoreCmdline" => n.ignore_cmdline = as_bool(val, &at)?,
            "buttonFeedbackMapper" => n.feedback = Some(parse_feedback_fn(as_str(val, &at)?).context(at.clone())?),
            "feedback" => {
                let o = as_obj(val, &at)?;
                let on = o.get("on").map(|v| as_u7(v, &format!("{at}.on"))).transpose()?.unwrap_or(127);
                let off = o.get("off").map(|v| as_u7(v, &format!("{at}.off"))).transpose()?.unwrap_or(0);
                n.feedback = Some(Feedback { on, off });
            }
            other => bail!("{note_at}: unknown key '{other}'"),
        }
    }
    if let Some(l) = local {
        n.local = Some(match l.as_str() {
            "encoderFine" => Local::EncoderFine,
            "encoderRough" => Local::EncoderRough,
            "attribute" => Local::Attribute(
                n.attribute.clone().ok_or_else(|| anyhow!("{at}: local \"attribute\" needs an \"attribute\" key"))?,
            ),
            other => bail!("{at}.local: unknown local function '{other}'"),
        });
    }
    Ok(n)
}

fn parse_relative(v: &Value, at: &str) -> Result<Relative> {
    let o = as_obj(v, at)?;
    for key in o.keys() {
        if !matches!(
            key.as_str(),
            "exec" | "currValue" | "posFrom" | "posTo" | "negFrom" | "negTo" | "returnChannel" | "returnFrom"
                | "returnTo" | "attribute" | "amount"
        ) {
            bail!("{at}: unknown key '{key}'");
        }
    }
    let req = |k: &str| -> Result<u8> {
        as_u7(o.get(k).ok_or_else(|| anyhow!("{at}: missing '{k}'"))?, &format!("{at}.{k}"))
    };
    let opt = |k: &str| -> Result<Option<u8>> { o.get(k).map(|v| as_u7(v, &format!("{at}.{k}"))).transpose() };
    let range = RelRange { pos_from: req("posFrom")?, pos_to: req("posTo")?, neg_from: req("negFrom")?, neg_to: req("negTo")? };

    let exec = match o.get("exec") {
        None => None,
        Some(e) => {
            let ring = match (opt("returnChannel")?, opt("returnFrom")?, opt("returnTo")?) {
                (Some(cc), Some(from), Some(to)) => Some(LedRing { cc, from, to }),
                (None, None, None) => None,
                _ => bail!("{at}: returnChannel, returnFrom and returnTo must be given together"),
            };
            Some(RelExec { exec: as_exec(e, &format!("{at}.exec"))?, initial: opt("currValue")?.unwrap_or(0), ring })
        }
    };
    let attribute = match o.get("attribute") {
        None => None,
        Some(a) => {
            let amount = match o.get("amount") {
                None => 1.0,
                Some(v) => v.as_f64().ok_or_else(|| anyhow!("{at}.amount: must be a number"))?,
            };
            Some(RelAttribute { attribute: as_str(a, &format!("{at}.attribute"))?.to_string(), amount })
        }
    };
    if exec.is_none() && attribute.is_none() {
        bail!("{at}: needs either 'exec' or 'attribute'");
    }
    Ok(Relative { range, exec, attribute })
}

/// Recognise `function(value) { if (value == 'On') { return N;} if (value == 'Off'){ return M;} ... }`.
fn parse_feedback_fn(src: &str) -> Result<Feedback> {
    fn find(src: &str, label: &str) -> Option<u8> {
        let compact: String = src.chars().filter(|c| !c.is_whitespace()).collect();
        let needle_sq = format!("value=='{label}'");
        let needle_dq = format!("value==\"{label}\"");
        let idx = compact.find(&needle_sq).or_else(|| compact.find(&needle_dq))?;
        let rest = &compact[idx..];
        let r = rest.find("return")? + "return".len();
        let digits: String = rest[r..].chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse::<u8>().ok().filter(|v| *v <= 127)
    }
    match (find(src, "On"), find(src, "Off")) {
        (Some(on), Some(off)) => Ok(Feedback { on, off }),
        _ => bail!(
            "buttonFeedbackMapper is not a simple On/Off lookup; replace it with \"feedback\": {{\"on\": N, \"off\": M}}"
        ),
    }
}

fn as_obj<'a>(v: &'a Value, at: &str) -> Result<&'a Map<String, Value>> {
    v.as_object().ok_or_else(|| anyhow!("{at}: must be an object"))
}
fn as_str<'a>(v: &'a Value, at: &str) -> Result<&'a str> {
    v.as_str().ok_or_else(|| anyhow!("{at}: must be a string"))
}
fn as_bool(v: &Value, at: &str) -> Result<bool> {
    v.as_bool().ok_or_else(|| anyhow!("{at}: must be true or false"))
}
fn as_int(v: &Value, at: &str) -> Result<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
    .ok_or_else(|| anyhow!("{at}: must be a whole number, got {v}"))
}
fn as_u7(v: &Value, at: &str) -> Result<u8> {
    let n = as_int(v, at)?;
    u8::try_from(n).ok().filter(|n| *n <= 127).ok_or_else(|| anyhow!("{at}: must be 0-127, got {n}"))
}
fn as_channel(v: &Value, at: &str) -> Result<u8> {
    let n = as_int(v, at)?;
    u8::try_from(n).ok().filter(|n| (1..=16).contains(n)).ok_or_else(|| anyhow!("{at}: must be a MIDI channel 1-16, got {n}"))
}
fn as_exec(v: &Value, at: &str) -> Result<Exec> {
    let n = as_int(v, at)?;
    u16::try_from(n).ok().filter(|n| (1..=9999).contains(n)).ok_or_else(|| anyhow!("{at}: executor must be 1-9999, got {n}"))
}
fn as_u7_key(k: &str, at: &str) -> Result<u8> {
    k.parse::<u8>().ok().filter(|n| *n <= 127).ok_or_else(|| anyhow!("{at}: key must be a MIDI number 0-127"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_shipped_mappings_load() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("mappings");
        let mut count = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().is_some_and(|e| e == "json") {
                load_file(&p).unwrap_or_else(|e| panic!("{e:#}"));
                count += 1;
            }
        }
        assert_eq!(count, 11);
    }

    #[test]
    fn feedback_functions_become_tables() {
        let f = parse_feedback_fn(
            "function(value) { if (value == 'On') { return 5;} if (value == 'Off'){ return 0;} return 0; }",
        )
        .unwrap();
        assert_eq!(f, Feedback { on: 5, off: 0 });
        assert!(parse_feedback_fn("function(v) { return v.length; }").is_err());
    }

    #[test]
    fn xtouch_gets_touch_notes() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("mappings/xTouch1.json");
        let m = load_file(&p).unwrap();
        assert!(m.mc_mode);
        assert_eq!(m.fader_touch.get(&104), Some(&201));
        assert_eq!(m.fader_touch.get(&112), Some(&209));
    }

    #[test]
    fn compact_does_not_get_touch_notes() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("mappings/xTouchCompact1.json");
        let m = load_file(&p).unwrap();
        assert!(m.fader_touch.is_empty());
        assert_eq!(m.control.get(&104), None);
        assert_eq!(m.notes.get(&104).unwrap().cmd.as_deref(), Some("Previous Page"));
    }

    #[test]
    fn typos_are_rejected_with_location() {
        let err = parse_legacy(r#"{"note": {"5": {"quicKy": "Go"}}}"#).unwrap_err();
        assert!(format!("{err:#}").contains("note.5: unknown key 'quicKy'"), "{err:#}");
        let err = parse_legacy(r#"{"control": {"200": 201}}"#).unwrap_err();
        assert!(format!("{err:#}").contains("control.200"), "{err:#}");
    }

    #[test]
    fn relative_out_of_range_is_none() {
        let r = RelRange { pos_from: 1, pos_to: 8, neg_from: 65, neg_to: 72 };
        assert_eq!(r.step(1), Some(1));
        assert_eq!(r.step(66), Some(-2));
        assert_eq!(r.step(30), None);
    }
}
