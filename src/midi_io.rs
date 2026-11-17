//! MIDI port management: finds the configured surfaces by port name, connects,
//! detects unplug/replug (polling the port list) and reports transitions.
//! Only state changes are logged, so a missing device does not flood the log.

use crate::Event;
use anyhow::{Result, anyhow};
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use std::sync::mpsc::Sender;
use tracing::{info, warn};

const CLIENT: &str = "ma3-osc";

#[derive(Debug, Clone)]
pub struct PortSpec {
    pub name: String,
    pub input: String,
    pub output: String,
}

struct Conn {
    in_name: String,
    out_name: String,
    _input: MidiInputConnection<()>,
    output: MidiOutputConnection,
}

struct Slot {
    spec: PortSpec,
    conn: Option<Conn>,
    send_failed: bool,
    problem: Option<String>,
}

impl Slot {
    fn note_problem(&mut self, msg: String) {
        if self.problem.as_ref() != Some(&msg) {
            warn!(device = %self.spec.name, "{msg}");
            self.problem = Some(msg);
        }
    }
}

pub struct MidiIo {
    lister_in: MidiInput,
    lister_out: MidiOutput,
    slots: Vec<Slot>,
    tx: Sender<Event>,
}

pub fn list_ports() -> Result<(Vec<String>, Vec<String>)> {
    let i = MidiInput::new(CLIENT).map_err(|e| anyhow!("MIDI init failed: {e}"))?;
    let o = MidiOutput::new(CLIENT).map_err(|e| anyhow!("MIDI init failed: {e}"))?;
    Ok((in_names(&i), out_names(&o)))
}

fn in_names(m: &MidiInput) -> Vec<String> {
    m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect()
}

fn out_names(m: &MidiOutput) -> Vec<String> {
    m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect()
}

/// Exact (case-insensitive) match first, then substring; skip ports already in use.
fn pick(ports: &[String], pattern: &str, claimed: &[String]) -> Option<String> {
    let pat = pattern.to_lowercase();
    let free = || ports.iter().filter(|p| !claimed.contains(p));
    free().find(|p| p.to_lowercase() == pat).or_else(|| free().find(|p| p.to_lowercase().contains(&pat))).cloned()
}

impl MidiIo {
    pub fn new(specs: Vec<PortSpec>, tx: Sender<Event>) -> Result<Self> {
        let lister_in = MidiInput::new(CLIENT).map_err(|e| anyhow!("MIDI init failed: {e}"))?;
        let lister_out = MidiOutput::new(CLIENT).map_err(|e| anyhow!("MIDI init failed: {e}"))?;
        let slots = specs.into_iter().map(|spec| Slot { spec, conn: None, send_failed: false, problem: None }).collect();
        Ok(MidiIo { lister_in, lister_out, slots, tx })
    }

    pub fn send(&mut self, idx: usize, bytes: &[u8]) {
        let Some(slot) = self.slots.get_mut(idx) else { return };
        let Some(conn) = slot.conn.as_mut() else { return };
        if slot.send_failed {
            return;
        }
        if let Err(e) = conn.output.send(bytes) {
            warn!(device = %slot.spec.name, "MIDI send failed: {e}");
            slot.send_failed = true;
        }
    }

    /// Check port presence; returns (device index, now connected?) transitions.
    pub fn scan(&mut self) -> Vec<(usize, bool)> {
        let ins = in_names(&self.lister_in);
        let outs = out_names(&self.lister_out);
        let mut changes = Vec::new();

        for (i, s) in self.slots.iter_mut().enumerate() {
            let gone = match &s.conn {
                Some(c) => s.send_failed || !ins.contains(&c.in_name) || !outs.contains(&c.out_name),
                None => false,
            };
            if gone {
                warn!(device = %s.spec.name, "MIDI ports gone, waiting for the device to come back");
                s.conn = None;
                s.send_failed = false;
                s.problem = None;
                changes.push((i, false));
            }
        }

        let mut claimed_in: Vec<String> = self.slots.iter().filter_map(|s| s.conn.as_ref().map(|c| c.in_name.clone())).collect();
        let mut claimed_out: Vec<String> =
            self.slots.iter().filter_map(|s| s.conn.as_ref().map(|c| c.out_name.clone())).collect();

        // More specific patterns first, so "X-Touch Compact" claims its port before "X-Touch" can.
        let mut order: Vec<usize> = (0..self.slots.len()).filter(|i| self.slots[*i].conn.is_none()).collect();
        order.sort_by_key(|i| std::cmp::Reverse(self.slots[*i].spec.input.len().max(self.slots[*i].spec.output.len())));

        for i in order {
            let spec = self.slots[i].spec.clone();
            let Some(in_name) = pick(&ins, &spec.input, &claimed_in) else {
                self.slots[i].note_problem(format!("no MIDI input matching \"{}\" (yet)", spec.input));
                continue;
            };
            let Some(out_name) = pick(&outs, &spec.output, &claimed_out) else {
                self.slots[i].note_problem(format!("no MIDI output matching \"{}\" (yet)", spec.output));
                continue;
            };
            match self.connect(i, &in_name, &out_name) {
                Ok(conn) => {
                    info!(device = %spec.name, input = %in_name, output = %out_name, "MIDI connected");
                    claimed_in.push(in_name);
                    claimed_out.push(out_name);
                    let s = &mut self.slots[i];
                    s.conn = Some(conn);
                    s.send_failed = false;
                    s.problem = None;
                    changes.push((i, true));
                }
                Err(e) => self.slots[i].note_problem(format!("cannot open MIDI ports: {e:#}")),
            }
        }
        changes
    }

    fn connect(&self, idx: usize, in_name: &str, out_name: &str) -> Result<Conn> {
        let mut mi = MidiInput::new(CLIENT).map_err(|e| anyhow!("{e}"))?;
        mi.ignore(Ignore::All);
        let ip = mi
            .ports()
            .into_iter()
            .find(|p| mi.port_name(p).ok().as_deref() == Some(in_name))
            .ok_or_else(|| anyhow!("input port disappeared"))?;
        let tx = self.tx.clone();
        let input = mi
            .connect(
                &ip,
                "ma3-osc-in",
                move |_stamp, bytes, _| {
                    let _ = tx.send(Event::Midi { dev: idx, bytes: bytes.to_vec() });
                },
                (),
            )
            .map_err(|e| anyhow!("input: {e}"))?;

        let mo = MidiOutput::new(CLIENT).map_err(|e| anyhow!("{e}"))?;
        let op = mo
            .ports()
            .into_iter()
            .find(|p| mo.port_name(p).ok().as_deref() == Some(out_name))
            .ok_or_else(|| anyhow!("output port disappeared"))?;
        let output = mo.connect(&op, "ma3-osc-out").map_err(|e| anyhow!("output: {e}"))?;
        Ok(Conn { in_name: in_name.into(), out_name: out_name.into(), _input: input, output })
    }
}

#[cfg(test)]
mod tests {
    use super::pick;

    #[test]
    fn compact_and_full_xtouch_are_told_apart() {
        let ports = vec!["X-TOUCH COMPACT".to_string(), "X-Touch".to_string()];
        assert_eq!(pick(&ports, "x-touch", &[]).as_deref(), Some("X-Touch"));
        assert_eq!(pick(&ports, "X-Touch Compact", &[]).as_deref(), Some("X-TOUCH COMPACT"));
        let only_compact = vec!["X-TOUCH COMPACT".to_string()];
        assert_eq!(pick(&only_compact, "X-Touch", &["X-TOUCH COMPACT".to_string()]), None);
    }
}
