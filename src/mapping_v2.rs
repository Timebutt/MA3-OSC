//! Mapping format v2: readable lists instead of number-keyed objects, explicit
//! surface type, no JavaScript, and an optional "label" on every entry.
//!
//! ```json
//! { "format": 2, "surface": "x-touch",
//!   "faders":  [ { "pitch": 1, "exec": 201, "label": "Fader 1" } ],
//!   "buttons": [ { "note": 104, "cmd": "Previous Page", "minVelocity": 101 } ],
//!   "strips":  [ { "strip": 0, "exec": 201 } ] }
//! ```
//!
//! Both formats load into the same `DeviceMapping`; `to_v2` converts legacy files.

use crate::mapping::{DeviceMapping, Exec, Feedback, LedRing, Local, NoteAction, RelAttribute, RelExec, RelRange, Relative};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingV2 {
    pub format: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub surface: SurfaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_channel: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button_feedback: Option<FeedbackV2>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub timecode_display: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faders: Vec<FaderV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fader_touch: Vec<TouchV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub encoders: Vec<EncoderV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<ButtonV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strips: Vec<StripV2>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SurfaceKind {
    /// Full X-Touch in Mackie Control mode (scribble strips, timecode display).
    XTouch,
    /// X-Touch Extender in Mackie Control mode (scribble strips, no timecode display).
    XTouchExtender,
    /// Any plain MIDI controller (X-Touch Compact, APC mini, Launchpad, ...).
    Midi,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedbackV2 {
    pub on: u8,
    pub off: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaderV2 {
    /// Pitch-bend channel 1-16 (motor faders in Mackie mode) ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<u8>,
    /// ... or a CC number (any channel in, channel 1 out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<u8>,
    pub exec: Exec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TouchV2 {
    pub note: u8,
    pub exec: Exec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepsV2 {
    /// Inclusive value range meaning "turned right" (first value = 1 step).
    pub up: [u8; 2],
    /// Inclusive value range meaning "turned left".
    pub down: [u8; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RingV2 {
    pub cc: u8,
    pub from: u8,
    pub to: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncoderV2 {
    pub cc: u8,
    pub steps: StepsV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<Exec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring: Option<RingV2>,
    /// Attribute name, or "current" for the attribute selected on the surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionV2 {
    EncoderFine,
    EncoderRough,
    SelectAttribute,
    TimecodeSelect,
    TimecodePlayPause,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ButtonV2 {
    pub note: u8,
    /// Output channel for LED feedback (standard MIDI devices only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<Exec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quickey: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionV2>,
    /// For "selectAttribute": the attribute; on its own: LED lights while it is selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,
    /// Only react when velocity >= this (e.g. 101 = press only, ignore release).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_velocity: Option<u8>,
    /// Fixed LED value that ignores feedback from MA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_led: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<FeedbackV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StripV2 {
    pub strip: u8,
    pub exec: Exec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ---------------------------------------------------------------- v2 -> model

fn u7(v: u8, at: &str) -> Result<u8> {
    if v > 127 {
        bail!("{at}: must be 0-127, got {v}");
    }
    Ok(v)
}
fn channel(v: u8, at: &str) -> Result<u8> {
    if !(1..=16).contains(&v) {
        bail!("{at}: must be a MIDI channel 1-16, got {v}");
    }
    Ok(v)
}
fn exec(v: Exec, at: &str) -> Result<Exec> {
    if !(1..=9999).contains(&v) {
        bail!("{at}: executor must be 1-9999, got {v}");
    }
    Ok(v)
}
fn insert_unique<V>(map: &mut BTreeMap<u8, V>, k: u8, v: V, at: &str, what: &str) -> Result<()> {
    if map.insert(k, v).is_some() {
        bail!("{at}: {what} {k} is mapped twice");
    }
    Ok(())
}

pub fn parse_v2(text: &str) -> Result<DeviceMapping> {
    let v: MappingV2 = serde_json::from_str(text).context("not a valid v2 mapping")?;
    from_v2(&v)
}

pub fn from_v2(v: &MappingV2) -> Result<DeviceMapping> {
    if v.format != 2 {
        bail!("format: expected 2, got {}", v.format);
    }
    let mut m = DeviceMapping {
        mc_mode: v.surface != SurfaceKind::Midi,
        extender: v.surface == SurfaceKind::XTouchExtender,
        timecode_display: v.timecode_display,
        default_channel: channel(v.midi_channel.unwrap_or(1), "midiChannel")?,
        ..Default::default()
    };
    if v.timecode_display && v.surface != SurfaceKind::XTouch {
        bail!("timecodeDisplay: only the full x-touch has a timecode display");
    }
    if let Some(f) = v.button_feedback {
        m.feedback = Feedback { on: u7(f.on, "buttonFeedback.on")?, off: u7(f.off, "buttonFeedback.off")? };
    }
    for (i, f) in v.faders.iter().enumerate() {
        let at = format!("faders[{i}]");
        let e = exec(f.exec, &at)?;
        match (f.pitch, f.cc) {
            (Some(ch), None) => insert_unique(&mut m.pitch, channel(ch, &format!("{at}.pitch"))?, e, &at, "pitch channel")?,
            (None, Some(cc)) => insert_unique(&mut m.control, u7(cc, &format!("{at}.cc"))?, e, &at, "CC")?,
            _ => bail!("{at}: needs exactly one of \"pitch\" or \"cc\""),
        }
    }
    for (i, t) in v.fader_touch.iter().enumerate() {
        let at = format!("faderTouch[{i}]");
        insert_unique(&mut m.fader_touch, u7(t.note, &at)?, exec(t.exec, &at)?, &at, "note")?;
    }
    for (i, s) in v.strips.iter().enumerate() {
        let at = format!("strips[{i}]");
        if s.strip > 7 {
            bail!("{at}.strip: must be 0-7, got {}", s.strip);
        }
        if v.surface == SurfaceKind::Midi {
            bail!("{at}: scribble strips need surface \"x-touch\" or \"x-touch-extender\"");
        }
        insert_unique(&mut m.display, s.strip, exec(s.exec, &at)?, &at, "strip")?;
    }
    for (i, e) in v.encoders.iter().enumerate() {
        let at = format!("encoders[{i}]");
        let range = RelRange {
            pos_from: u7(e.steps.up[0], &at)?,
            pos_to: u7(e.steps.up[1], &at)?,
            neg_from: u7(e.steps.down[0], &at)?,
            neg_to: u7(e.steps.down[1], &at)?,
        };
        let rel_exec = match e.exec {
            Some(x) => Some(RelExec {
                exec: exec(x, &at)?,
                initial: u7(e.start.unwrap_or(0), &format!("{at}.start"))?,
                ring: match &e.ring {
                    Some(r) => Some(LedRing { cc: u7(r.cc, &at)?, from: u7(r.from, &at)?, to: u7(r.to, &at)? }),
                    None => None,
                },
            }),
            None if e.ring.is_some() || e.start.is_some() => bail!("{at}: \"ring\" and \"start\" need an \"exec\""),
            None => None,
        };
        let attribute = match &e.attribute {
            Some(a) => Some(RelAttribute { attribute: a.clone(), amount: e.amount.unwrap_or(1.0) }),
            None if e.amount.is_some() => bail!("{at}: \"amount\" needs an \"attribute\""),
            None => None,
        };
        if rel_exec.is_none() && attribute.is_none() {
            bail!("{at}: needs \"exec\" or \"attribute\"");
        }
        insert_unique(&mut m.relative, u7(e.cc, &at)?, Relative { range, exec: rel_exec, attribute }, &at, "CC")?;
    }
    for (i, b) in v.buttons.iter().enumerate() {
        let at = format!("buttons[{i}]");
        let mut n = NoteAction {
            channel: b.channel.map(|c| channel(c, &format!("{at}.channel"))).transpose()?,
            exec: b.exec.map(|x| exec(x, &at)).transpose()?,
            cmd: b.cmd.clone(),
            quickey: b.quickey.clone(),
            attribute: b.attribute.clone(),
            permanent_feedback: b.fixed_led.map(|x| u7(x, &format!("{at}.fixedLed"))).transpose()?.filter(|x| *x > 0),
            feedback: b
                .feedback
                .map(|f| Ok::<_, anyhow::Error>(Feedback { on: u7(f.on, &at)?, off: u7(f.off, &at)? }))
                .transpose()?,
            ..Default::default()
        };
        if let Some(mv) = b.min_velocity {
            if !(1..=128).contains(&mv) {
                bail!("{at}.minVelocity: must be 1-128, got {mv}");
            }
            n.min_value = Some(mv - 1).filter(|x| *x > 0);
        }
        match b.action {
            Some(ActionV2::EncoderFine) => n.local = Some(Local::EncoderFine),
            Some(ActionV2::EncoderRough) => n.local = Some(Local::EncoderRough),
            Some(ActionV2::SelectAttribute) => {
                let a = b.attribute.clone().ok_or_else(|| anyhow!("{at}: \"selectAttribute\" needs \"attribute\""))?;
                n.local = Some(Local::Attribute(a));
            }
            Some(ActionV2::TimecodeSelect) => n.timecode_select = true,
            Some(ActionV2::TimecodePlayPause) => n.timecode_play_pause = true,
            None => {}
        }
        insert_unique(&mut m.notes, u7(b.note, &at)?, n, &at, "note")?;
    }
    crate::mapping::add_mackie_touch(&mut m);
    Ok(m)
}

// ---------------------------------------------------------------- model -> v2

pub fn to_v2(m: &DeviceMapping) -> Result<MappingV2> {
    let surface = match (m.mc_mode, m.extender) {
        (false, _) => SurfaceKind::Midi,
        (true, false) => SurfaceKind::XTouch,
        (true, true) => SurfaceKind::XTouchExtender,
    };
    let mut faders: Vec<FaderV2> =
        m.pitch.iter().map(|(ch, e)| FaderV2 { pitch: Some(*ch), cc: None, exec: *e, label: None }).collect();
    faders.extend(m.control.iter().map(|(cc, e)| FaderV2 { pitch: None, cc: Some(*cc), exec: *e, label: None }));

    let encoders = m
        .relative
        .iter()
        .map(|(cc, r)| EncoderV2 {
            cc: *cc,
            steps: StepsV2 { up: [r.range.pos_from, r.range.pos_to], down: [r.range.neg_from, r.range.neg_to] },
            exec: r.exec.as_ref().map(|x| x.exec),
            start: r.exec.as_ref().map(|x| x.initial).filter(|v| *v != 0),
            ring: r.exec.as_ref().and_then(|x| x.ring.as_ref()).map(|r| RingV2 { cc: r.cc, from: r.from, to: r.to }),
            attribute: r.attribute.as_ref().map(|a| a.attribute.clone()),
            amount: r.attribute.as_ref().map(|a| a.amount),
            label: None,
        })
        .collect();

    let mut buttons = Vec::new();
    for (note, n) in &m.notes {
        let mut actions = Vec::new();
        match &n.local {
            Some(Local::EncoderFine) => actions.push(ActionV2::EncoderFine),
            Some(Local::EncoderRough) => actions.push(ActionV2::EncoderRough),
            Some(Local::Attribute(_)) => actions.push(ActionV2::SelectAttribute),
            None => {}
        }
        if n.timecode_select {
            actions.push(ActionV2::TimecodeSelect);
        }
        if n.timecode_play_pause {
            actions.push(ActionV2::TimecodePlayPause);
        }
        if actions.len() > 1 {
            bail!("note {note}: v2 allows one local action per button, this one has {actions:?}");
        }
        let attribute = match &n.local {
            Some(Local::Attribute(a)) => Some(a.clone()),
            _ => n.attribute.clone(),
        };
        buttons.push(ButtonV2 {
            note: *note,
            channel: n.channel,
            exec: n.exec,
            cmd: n.cmd.clone(),
            quickey: n.quickey.clone(),
            action: actions.first().copied(),
            attribute,
            min_velocity: n.min_value.map(|v| v + 1),
            fixed_led: n.permanent_feedback,
            feedback: n.feedback.map(|f| FeedbackV2 { on: f.on, off: f.off }),
            label: None,
        });
    }

    // Touch notes that Mackie mode adds automatically need not be written out.
    let mut auto = DeviceMapping { mc_mode: m.mc_mode, pitch: m.pitch.clone(), notes: m.notes.clone(), ..Default::default() };
    crate::mapping::add_mackie_touch(&mut auto);
    let fader_touch = m
        .fader_touch
        .iter()
        .filter(|(n, e)| auto.fader_touch.get(n) != Some(e))
        .map(|(n, e)| TouchV2 { note: *n, exec: *e, label: None })
        .collect();

    Ok(MappingV2 {
        format: 2,
        label: None,
        surface,
        midi_channel: Some(m.default_channel).filter(|c| *c != 1),
        button_feedback: Some(FeedbackV2 { on: m.feedback.on, off: m.feedback.off })
            .filter(|_| m.feedback != Feedback::default()),
        timecode_display: m.timecode_display,
        faders,
        fader_touch,
        encoders,
        buttons,
        strips: m.display.iter().map(|(s, e)| StripV2 { strip: *s, exec: *e, label: None }).collect(),
    })
}

/// Pretty JSON with one line per list entry, so long mappings stay readable.
pub fn to_json(v: &MappingV2) -> Result<String> {
    let value = serde_json::to_value(v)?;
    let obj = value.as_object().ok_or_else(|| anyhow!("internal: mapping is not an object"))?;
    let mut out = String::from("{\n");
    let n = obj.len();
    for (i, (k, val)) in obj.iter().enumerate() {
        let comma = if i + 1 < n { "," } else { "" };
        match val {
            serde_json::Value::Array(items) => {
                out.push_str(&format!("    \"{k}\": [\n"));
                for (j, item) in items.iter().enumerate() {
                    let c = if j + 1 < items.len() { "," } else { "" };
                    out.push_str(&format!("        {}{c}\n", one_line(item)));
                }
                out.push_str(&format!("    ]{comma}\n"));
            }
            other => out.push_str(&format!("    \"{k}\": {}{comma}\n", one_line(other))),
        }
    }
    out.push_str("}\n");
    Ok(out)
}

/// Compact JSON with a space after ':' and ',' (outside strings).
fn one_line(v: &serde_json::Value) -> String {
    let compact = v.to_string();
    let mut out = String::with_capacity(compact.len() + 16);
    let (mut in_str, mut escaped) = (false, false);
    for c in compact.chars() {
        out.push(c);
        if in_str {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_str = false,
                _ => {}
            }
        } else if c == '"' {
            in_str = true;
        } else if c == ':' || c == ',' {
            out.push(' ');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::load_file;
    use std::path::Path;

    #[test]
    fn every_shipped_mapping_converts_losslessly() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("mappings");
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let legacy = load_file(&p).unwrap();
            let text = to_json(&to_v2(&legacy).unwrap()).unwrap();
            let back = parse_v2(&text).unwrap_or_else(|e| panic!("{}: {e:#}\n{text}", p.display()));
            assert_eq!(legacy, back, "{}", p.display());
        }
    }

    #[test]
    fn readable_output() {
        let m = crate::mapping::parse_legacy(r#"{"mode":"mc","pitch":{"1":201},"note":{"104":{"cmd":"Go, \"x\"","minValue":100}}}"#)
            .unwrap();
        let text = to_json(&to_v2(&m).unwrap()).unwrap();
        assert!(text.contains(r#"{"pitch": 1, "exec": 201}"#), "{text}");
        assert!(text.contains(r#"{"note": 104, "cmd": "Go, \"x\"", "minVelocity": 101}"#), "{text}");
    }

    #[test]
    fn mistakes_are_reported() {
        let err = |s: &str| format!("{:#}", parse_v2(s).unwrap_err());
        assert!(err(r#"{"format":2,"surface":"midi","buttons":[{"note":1,"exc":5}]}"#).contains("exc"));
        assert!(err(r#"{"format":2,"surface":"midi","buttons":[{"note":1},{"note":1}]}"#).contains("buttons[1]"));
        assert!(err(r#"{"format":2,"surface":"midi","strips":[{"strip":0,"exec":201}]}"#).contains("strips[0]"));
        assert!(err(r#"{"format":2,"surface":"x-touch","faders":[{"exec":201}]}"#).contains("faders[0]"));
        assert!(err(r#"{"format":2,"surface":"xtouch"}"#).contains("xtouch"));
    }
}
