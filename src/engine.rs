//! The bridge logic: MIDI from surfaces -> OSC to grandMA3, and OSC feedback
//! from grandMA3 -> surface state. Pure and single-threaded: all I/O goes
//! through the `Sink` trait and time is passed in, so it is fully testable.
//!
//! Behaviour follows the original pam-OSC open-stage-control module, with
//! these deliberate fixes:
//!  * fader feedback is held while a fader is touched / just moved (no motor fights),
//!  * relative encoder values outside the configured ranges are ignored
//!    (they used to reset the fader to 0),
//!  * LCD text is sanitised to 7-bit ASCII (accents used to corrupt the SysEx),
//!  * LCD names/colors are tracked per device (needed for X-Touch + Extender),
//!  * timecode play/pause on an empty slot no longer crashes,
//!  * MIDI from a port without a mapping is ignored instead of throwing,
//!  * optional heartbeat with MA: strips show "MA LINK / LOST" when MA goes quiet.

use crate::mapping::{DeviceMapping, Exec, Feedback, Local};
use crate::midi::MidiMsg;
use crate::surface::{OutKey, OutVal, Surface};
use crate::xtouch;
use rosc::OscType;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tracing::{debug, info, trace, warn};

pub trait Sink {
    fn midi(&mut self, device: usize, bytes: &[u8]);
    fn osc(&mut self, addr: &str, args: Vec<OscType>);
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub prefix: String,
    /// Must equal "FaderRange" on MA's OSC line: the value MA reads as 100%.
    /// Internally faders are 0..127; outgoing values are scaled to 0..fader_range.
    pub fader_range: u16,
    pub echo_guard: Duration,
    pub heartbeat_timeout: Duration,
    pub refresh_every: Option<Duration>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            prefix: String::new(),
            fader_range: 100,
            echo_guard: Duration::from_millis(300),
            heartbeat_timeout: Duration::from_millis(3000),
            refresh_every: Some(Duration::from_secs(10)),
        }
    }
}

const BLANK: [u8; xtouch::STRIP_CHARS] = [b' '; xtouch::STRIP_CHARS];
const TIMECODE_LONG_PRESS: Duration = Duration::from_millis(500);
const RELOAD_MIN_INTERVAL: Duration = Duration::from_secs(2);
/// MA global variable the plugin watches for an executor that completes the command line.
const CMDLINE_TARGET_VAR: &str = "ma3OscTarget";

/// How long after a motor command the fader may still be travelling. Surfaces
/// without touch sensing (X-Touch Compact) report their position while the
/// motor moves; such reports are not forwarded to MA as hand moves.
const MOTOR_TRAVEL: Duration = Duration::from_millis(500);

/// A motor move in progress: incoming positions within `lo..=hi` are the motor.
#[derive(Debug, Clone, Copy)]
struct Travel {
    lo: u8,
    hi: u8,
    until: Instant,
}

pub struct Device {
    pub name: String,
    pub map: DeviceMapping,
    surface: Surface,
    rel_value: HashMap<u8, u8>,
    /// Last value forwarded to MA per fader, for the jitter filter.
    fader_in: HashMap<OutKey, FaderIn>,
    /// Motor moves still in progress, per fader.
    travel: HashMap<OutKey, Travel>,
    names: [([u8; 7], [u8; 7]); 8],
    colors: [u8; 8],
    /// Executor buttons whose press went to MA's command line: their release is dropped.
    to_cmdline: HashSet<u8>,
}

/// Hysteresis for absolute faders and encoders (0..127 scale).
///
/// A resting or motor-positioned fader often wobbles between two neighbouring
/// values; forwarding each wobble floods MA and can flip X-fade executors back
/// and forth. A change continuing in the direction of the last move passes at
/// full resolution; a reversal by a single step is ignored. Repeated values are
/// dropped, and 0 and 127 always pass so a fader can always reach its ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FaderIn {
    last: u8,
    dir: i8,
}

fn fader_filter(state: Option<FaderIn>, v: u8) -> Option<FaderIn> {
    let Some(st) = state else { return Some(FaderIn { last: v, dir: 0 }) };
    if v == st.last {
        return None;
    }
    let delta = v as i16 - st.last as i16;
    let dir = delta.signum() as i8;
    if v == 0 || v == 127 || delta.abs() >= 2 || dir == st.dir {
        Some(FaderIn { last: v, dir })
    } else {
        None
    }
}

#[derive(Debug, Default, Clone)]
struct Slot {
    hrs: String,
    mins: String,
    secs: String,
    mili: String,
    running: bool,
    cleared: bool,
}

#[derive(Debug, Clone, Copy)]
enum Led {
    On,
    Off,
    Raw(u8),
}

pub struct Engine {
    cfg: EngineConfig,
    devices: Vec<Device>,
    page: String,
    /// Executors whose feedback arrives under another page than `page`: they
    /// are set to Fix on that page. Input for them goes to that page too.
    exec_page: HashMap<Exec, String>,
    desk_locked: bool,
    encoder_fine: bool,
    encoder_rough: bool,
    current_attribute: String,
    tc_selected: u8,
    tc_slots: HashMap<u8, Slot>,
    tc_press_deadline: Option<(Instant, u8)>,
    heartbeat_seen: bool,
    last_from_ma: Option<Instant>,
    ma_lost: bool,
    last_reload_request: Option<Instant>,
    last_refresh: Instant,
    /// MA's command line holds a command waiting for its destination (reported by the plugin).
    cmdline_pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaLink {
    /// Nothing received from grandMA3 yet.
    Waiting,
    /// Data arrives, but the plugin sends no heartbeat (old pam-OSC plugin).
    NoHeartbeat,
    Ok,
    Lost,
}

impl Engine {
    pub fn new(cfg: EngineConfig, devices: Vec<(String, DeviceMapping)>, now: Instant) -> Self {
        let devices = devices
            .into_iter()
            .map(|(name, map)| Device {
                surface: Surface::new(if map.extender { xtouch::MC_EXTENDER_ID } else { xtouch::MC_DEVICE_ID }),
                name,
                map,
                rel_value: HashMap::new(),
                fader_in: HashMap::new(),
                travel: HashMap::new(),
                names: [(BLANK, BLANK); 8],
                colors: [xtouch::COLOR_OFF; 8],
                to_cmdline: HashSet::new(),
            })
            .collect::<Vec<_>>();
        let mut e = Engine {
            cfg,
            devices,
            page: "1".into(),
            exec_page: HashMap::new(),
            desk_locked: false,
            encoder_fine: false,
            encoder_rough: false,
            current_attribute: "dimmer".into(),
            tc_selected: 0,
            tc_slots: HashMap::new(),
            tc_press_deadline: None,
            heartbeat_seen: false,
            last_from_ma: None,
            ma_lost: false,
            last_reload_request: None,
            last_refresh: now,
            cmdline_pending: false,
        };
        for d in 0..e.devices.len() {
            let dev = &mut e.devices[d];
            for (cc, r) in &dev.map.relative {
                if let Some(x) = &r.exec {
                    dev.rel_value.insert(*cc, x.initial);
                }
            }
        }
        e
    }

    #[cfg(test)]
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// Initial surface state (LEDs, displays) and ask MA for a full update.
    pub fn start(&mut self, sink: &mut dyn Sink, now: Instant) {
        let mut outs = self.outs();
        self.attribute_leds(&mut outs);
        for d in 0..self.devices.len() {
            let notes: Vec<(u8, u8, u8)> = self.devices[d]
                .map
                .notes
                .iter()
                .filter_map(|(n, a)| a.permanent_feedback.map(|pf| (*n, a.channel.unwrap_or(self.devices[d].map.default_channel), pf)))
                .collect();
            for (note, ch, pf) in notes {
                self.led(d, note, ch, Led::Raw(pf), None, &mut outs[d]);
            }
            self.render_display(d, &mut outs[d]);
        }
        self.timecode_reset_all(&mut outs);
        self.track_motors(now);
        self.flush(outs, sink);
        self.request_reload(sink, now);
    }

    pub fn device_connected(&mut self, idx: usize, sink: &mut dyn Sink, now: Instant) {
        let Some(dev) = self.devices.get_mut(idx) else { return };
        info!(device = %dev.name, "surface connected, sending full state");
        let mut out = vec![];
        dev.surface.connect(&mut out);
        self.track_motors(now);
        for b in out {
            sink.midi(idx, &b);
        }
        self.request_reload(sink, now);
    }

    pub fn device_disconnected(&mut self, idx: usize) {
        if let Some(dev) = self.devices.get_mut(idx) {
            warn!(device = %dev.name, "surface disconnected");
            dev.surface.disconnect();
            dev.fader_in.clear();
            dev.travel.clear();
            dev.to_cmdline.clear();
        }
    }

    #[cfg(test)]
    pub fn ma_link_lost(&self) -> bool {
        self.ma_lost
    }

    /// Link state for the status display.
    pub fn ma_link(&self) -> MaLink {
        match (self.last_from_ma.is_some(), self.heartbeat_seen, self.ma_lost) {
            (false, _, _) => MaLink::Waiting,
            (true, false, _) => MaLink::NoHeartbeat,
            (true, true, false) => MaLink::Ok,
            (true, true, true) => MaLink::Lost,
        }
    }

    /// Periodic housekeeping; call at least every ~20 ms.
    pub fn tick(&mut self, sink: &mut dyn Sink, now: Instant) {
        let mut outs = self.outs();
        for (d, dev) in self.devices.iter_mut().enumerate() {
            dev.surface.expire_holds(now, &mut outs[d]);
        }

        if let Some((deadline, slot_no)) = self.tc_press_deadline
            && now >= deadline
        {
            self.tc_press_deadline = None;
            if let Some(slot) = self.tc_slots.get_mut(&slot_no) {
                slot.running = false;
                slot.cleared = true;
            }
            self.send_cmd(sink, &format!("Off Timecodeslot {slot_no}"));
        }

        if self.heartbeat_seen
            && !self.ma_lost
            && self.last_from_ma.is_some_and(|t| now.duration_since(t) > self.cfg.heartbeat_timeout)
        {
            warn!("no heartbeat from grandMA3 for {:?}: link LOST", self.cfg.heartbeat_timeout);
            self.ma_lost = true;
            self.cmdline_pending = false;
            for d in 0..self.devices.len() {
                self.render_display(d, &mut outs[d]);
            }
        }

        if let Some(every) = self.cfg.refresh_every
            && now.duration_since(self.last_refresh) >= every
        {
            self.last_refresh = now;
            for (d, dev) in self.devices.iter_mut().enumerate() {
                dev.surface.resync(&mut outs[d]);
            }
        }
        self.track_motors(now);
        self.flush(outs, sink);
    }

    // ------------------------------------------------------------------ MIDI in

    pub fn midi_in(&mut self, d: usize, bytes: &[u8], sink: &mut dyn Sink, now: Instant) {
        if d >= self.devices.len() {
            return;
        }
        let Some(msg) = MidiMsg::parse(bytes) else { return };
        debug!(device = %self.devices[d].name, ?msg, "MIDI in");
        if self.desk_locked {
            debug!("desk locked: ignoring MIDI input");
            return;
        }
        let mut outs = self.outs();
        match msg {
            MidiMsg::Cc { cc, val, .. } => self.on_cc(d, cc, val, sink, now, &mut outs),
            MidiMsg::Pitch { ch, val } => {
                if let Some(exec) = self.devices[d].map.pitch.get(&ch).copied() {
                    let v = ((val as f64 / 16383.0) * 127.0).round() as u8;
                    let key = OutKey::Pitch { ch };
                    if !self.motor_echo(d, key, v, now) && self.fader_passes(d, key, v) {
                        let until = now + self.cfg.echo_guard;
                        self.devices[d].surface.local_change(key, OutVal::U14(val), until);
                        self.send_fader(sink, exec, v as i32);
                    }
                }
            }
            MidiMsg::Note { ch, note, vel } => self.on_note(d, ch, note, vel, sink, now, &mut outs),
            MidiMsg::SysEx(_) => {}
        }
        self.flush(outs, sink);
    }

    fn on_cc(&mut self, d: usize, cc: u8, val: u8, sink: &mut dyn Sink, now: Instant, outs: &mut [Vec<Vec<u8>>]) {
        let until = now + self.cfg.echo_guard;
        if let Some(exec) = self.devices[d].map.control.get(&cc).copied() {
            let key = OutKey::Cc { ch: 1, cc };
            if !self.motor_echo(d, key, val, now) && self.fader_passes(d, key, val) {
                self.devices[d].surface.local_change(key, OutVal::U7(val), until);
                self.send_fader(sink, exec, val as i32);
            }
        }
        let Some(rel) = self.devices[d].map.relative.get(&cc).cloned() else { return };
        let Some(step) = rel.range.step(val) else {
            debug!(cc, val, "relative encoder value outside configured ranges, ignored");
            return;
        };

        if let Some(x) = &rel.exec {
            if x.exec > 300 {
                self.osc_int(sink, &format!("/Page{}/Encoder{}", self.page_of(x.exec), x.exec), step);
            }
            let cur = self.devices[d].rel_value.get(&cc).copied().unwrap_or(0) as i32;
            let new = (cur + step).clamp(0, 127) as u8;
            self.devices[d].rel_value.insert(cc, new);
            self.send_fader(sink, x.exec, new as i32);
            if let Some(ring) = &x.ring {
                let v = map_range(new as f64, 0.0, 127.0, ring.from as f64, ring.to as f64);
                self.devices[d].surface.set(OutKey::Cc { ch: 1, cc: ring.cc }, OutVal::U7(v), &mut outs[d]);
            }
        }

        if let Some(a) = &rel.attribute {
            let mut change = step as f64 * a.amount;
            if self.encoder_fine {
                change /= 10.0;
            }
            if self.encoder_rough {
                change *= 10.0;
            }
            let sign = if change > 0.0 { "+" } else { "-" };
            let attr = if a.attribute == "current" { self.current_attribute.clone() } else { a.attribute.clone() };
            self.send_cmd(sink, &format!("Attribute {attr} at {sign} {}", change.abs()));
        }
    }

    /// True if `v` is the motor travelling to a position MA asked for, not a hand.
    fn motor_echo(&mut self, d: usize, key: OutKey, v: u8, now: Instant) -> bool {
        let dev = &mut self.devices[d];
        let Some(t) = dev.travel.get(&key).copied() else { return false };
        if now >= t.until {
            dev.travel.remove(&key);
            return false;
        }
        let echo = v >= t.lo.saturating_sub(1) && v <= t.hi.saturating_add(1);
        if echo {
            trace!(device = %dev.name, ?key, v, "motor travel ignored");
        }
        echo
    }

    /// Record the motor moves the surfaces were just sent (see `motor_echo`).
    /// The jitter filter restarts at the target, so settling wobble is ignored.
    fn track_motors(&mut self, now: Instant) {
        for dev in &mut self.devices {
            for (key, prev, new) in dev.surface.take_moves() {
                let is_fader = match key {
                    OutKey::Cc { ch: 1, cc } => dev.map.control.contains_key(&cc),
                    OutKey::Pitch { ch } => dev.map.pitch.contains_key(&ch),
                    _ => false,
                };
                let Some(to) = as_u7(&new) else { continue };
                if !is_fader {
                    continue;
                }
                // unknown start position: the motor may come from anywhere
                let (mut lo, mut hi) = match prev.as_ref().and_then(as_u7).or(dev.fader_in.get(&key).map(|f| f.last)) {
                    Some(from) => (from.min(to), from.max(to)),
                    None => (0, 127),
                };
                if let Some(t) = dev.travel.get(&key).filter(|t| t.until > now) {
                    lo = lo.min(t.lo);
                    hi = hi.max(t.hi);
                }
                dev.travel.insert(key, Travel { lo, hi, until: now + MOTOR_TRAVEL });
                dev.fader_in.insert(key, FaderIn { last: to, dir: 0 });
            }
        }
    }

    /// Jitter filter for an absolute fader value; true = forward it to MA.
    fn fader_passes(&mut self, d: usize, key: OutKey, v: u8) -> bool {
        let dev = &mut self.devices[d];
        match fader_filter(dev.fader_in.get(&key).copied(), v) {
            Some(st) => {
                dev.fader_in.insert(key, st);
                true
            }
            None => {
                trace!(device = %dev.name, ?key, v, "fader jitter ignored");
                false
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_note(&mut self, d: usize, _ch: u8, note: u8, vel: u8, sink: &mut dyn Sink, now: Instant, outs: &mut [Vec<Vec<u8>>]) {
        let action = self.devices[d].map.notes.get(&note).cloned();
        let Some(a) = action else {
            if let Some(exec) = self.devices[d].map.fader_touch.get(&note).copied() {
                self.touch(d, exec, vel > 0, now);
            }
            return;
        };
        if a.min_value.is_some_and(|m| vel <= m) {
            return;
        }

        if self.devices[d].map.timecode_display {
            if a.timecode_select {
                let slot = (self.tc_selected + 1) % 9;
                self.tc_selected = slot;
                self.timecode_reset_all(outs);
                if let Some(s) = self.tc_slots.get(&slot).cloned() {
                    self.timecode_show(&s, outs);
                }
            }
            if a.timecode_play_pause && self.tc_selected != 0 {
                let slot_no = self.tc_selected;
                if vel > 0 {
                    self.tc_press_deadline = Some((now + TIMECODE_LONG_PRESS, slot_no));
                } else {
                    self.tc_press_deadline = None;
                    let cmd = match self.tc_slots.get_mut(&slot_no) {
                        Some(s) if s.cleared => {
                            s.cleared = false;
                            None
                        }
                        Some(s) if s.running => {
                            s.running = false;
                            Some("Pause")
                        }
                        Some(s) => {
                            s.running = true;
                            Some("Go+")
                        }
                        None => None,
                    };
                    if let Some(c) = cmd {
                        self.send_cmd(sink, &format!("{c} Timecodeslot {slot_no}"));
                    }
                }
            }
        }

        if let Some(exec) = a.exec {
            if vel > 0 && self.cmdline_pending && !a.ignore_cmdline {
                // Like an executor key on a console: a command waiting in MA's
                // command line ("Assign Sequence 5") gets this executor as its
                // destination. OSC key presses can't do that, so the plugin
                // finishes the command; this tells it which executor was pressed.
                let target = format!("Page {}.{}", self.page_of(exec), exec);
                info!(%target, "executor pressed with a pending command line");
                self.send_cmd(sink, &format!("SetGlobalVariable \"{CMDLINE_TARGET_VAR}\" \"{target}\""));
                self.devices[d].to_cmdline.insert(note);
            } else if vel == 0 && self.devices[d].to_cmdline.remove(&note) {
                // release of a press that went to the command line
            } else {
                self.osc_int(sink, &format!("/Page{}/Key{}", self.page_of(exec), exec), vel as i32);
            }
        }
        if let Some(q) = &a.quickey {
            self.send_cmd(sink, &format!("Go+ Quickey \"{q}\""));
        }
        if let Some(c) = &a.cmd {
            self.send_cmd(sink, c);
        }
        match &a.local {
            Some(Local::EncoderFine) => {
                self.encoder_fine = !self.encoder_fine;
                let led = if self.encoder_fine { Led::On } else { Led::Off };
                self.led(d, note, 1, led, None, &mut outs[d]);
            }
            Some(Local::EncoderRough) => {
                self.encoder_rough = !self.encoder_rough;
                let led = if self.encoder_rough { Led::On } else { Led::Off };
                self.led(d, note, 1, led, None, &mut outs[d]);
            }
            Some(Local::Attribute(attr)) => {
                self.current_attribute = attr.clone();
                self.attribute_leds(outs);
            }
            None => {}
        }
    }

    fn touch(&mut self, d: usize, exec: Exec, down: bool, now: Instant) {
        let keys = fader_keys(&self.devices[d].map, exec);
        let until = now + self.cfg.echo_guard;
        for k in keys {
            if down {
                self.devices[d].surface.hold_touch(k);
            } else {
                self.devices[d].surface.release_touch(k, until);
            }
        }
    }

    // ------------------------------------------------------------------ OSC in

    pub fn osc_in(&mut self, addr: &str, args: &[OscType], sink: &mut dyn Sink, now: Instant) {
        debug!(addr, ?args, "OSC in");
        self.last_from_ma = Some(now);
        let mut outs = self.outs();

        if addr == "/pam/heartbeat" {
            if !self.heartbeat_seen {
                info!("grandMA3 heartbeat detected; link monitoring enabled");
            }
            self.heartbeat_seen = true;
        }
        if self.ma_lost {
            info!("grandMA3 link restored");
            self.ma_lost = false;
            for d in 0..self.devices.len() {
                self.render_display(d, &mut outs[d]);
            }
            self.request_reload(sink, now);
        }

        if addr == "/status/deskLocked" {
            if let Some(OscType::Bool(b)) = args.first() {
                if self.desk_locked != *b {
                    info!(locked = *b, "desk lock changed");
                }
                self.desk_locked = *b;
            }
            self.flush(outs, sink);
            return;
        }

        if addr == "/cmdline" {
            let text = args.first().and_then(arg_str).unwrap_or_default();
            let pending = !text.trim().is_empty();
            if pending != self.cmdline_pending {
                debug!(%text, "MA command line changed");
            }
            self.cmdline_pending = pending;
            self.flush(outs, sink);
            return;
        }

        let parts: Vec<&str> = addr.split('/').collect();
        let seg1 = parts.get(1).copied().unwrap_or("");
        let seg2 = parts.get(2).copied().unwrap_or("");
        let (kind, exec) = split_trailing_number(seg2);

        if let Some(exec) = exec {
            if let Some(p) = seg1.strip_prefix("Page").filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
                if p == self.page {
                    self.exec_page.remove(&exec);
                } else if self.exec_page.get(&exec).map(String::as_str) != Some(p) {
                    debug!(exec, page = p, "executor is fixed on another page");
                    self.exec_page.insert(exec, p.to_string());
                }
            }
            if kind.contains("Fader") {
                if let Some(v) = args.first().and_then(arg_f64) {
                    self.fader_feedback(exec, v.clamp(0.0, 127.0), &mut outs);
                }
            } else if kind.contains("Button") {
                let on = args.first().and_then(arg_str).is_some_and(|s| s == "On");
                self.button_feedback(exec, on, &mut outs);
            } else if kind.contains("Color") {
                if let Some(s) = args.first().and_then(arg_str) {
                    self.color_feedback(exec, &s, &mut outs);
                }
            } else if kind.contains("Name") {
                if let Some(s) = args.first().and_then(arg_str) {
                    self.name_feedback(exec, &s, &mut outs);
                }
            }
        }

        if addr.contains("/updatePage/current") {
            if let Some(p) = args.first().and_then(arg_f64) {
                let page = format!("{}", p.round() as i64);
                if page != self.page {
                    // the plugin re-sends every executor after a page change
                    self.exec_page.clear();
                }
                self.page = page;
                debug!(page = %self.page, "page changed");
            }
        }

        if seg1.contains("masterEnabled") && !seg2.is_empty() {
            let on = args.first().and_then(arg_f64).is_some_and(|v| v != 0.0);
            self.master_feedback(seg2, on, &mut outs);
        }

        if self.devices.iter().any(|d| d.map.timecode_display) {
            if seg1.contains("Timecode") {
                if let (_, Some(slot)) = split_trailing_number(seg1)
                    && let Some(t) = args.first().and_then(arg_str)
                {
                    self.timecode_feedback(slot as u8, &t, &mut outs);
                }
            }
            if let Some(n) = seg1.strip_prefix("14.")
                && let Ok(slot) = n.parse::<u8>()
                && let Some(s) = self.tc_slots.get_mut(&slot)
            {
                s.running = args.first().and_then(arg_str).is_some_and(|v| v == "Go+");
            }
        }
        self.track_motors(now);
        self.flush(outs, sink);
    }

    fn fader_feedback(&mut self, exec: Exec, v: f64, outs: &mut [Vec<Vec<u8>>]) {
        for (d, dev) in self.devices.iter_mut().enumerate() {
            for (cc, e) in &dev.map.control {
                if *e == exec {
                    let key = OutKey::Cc { ch: 1, cc: *cc };
                    let v = v.round().clamp(0.0, 127.0) as u8;
                    dev.surface.set(key, OutVal::U7(v), &mut outs[d]);
                }
            }
            for (ch, e) in &dev.map.pitch {
                if *e == exec {
                    let p = ((v / 127.0) * 16383.0).round() as u16;
                    let key = OutKey::Pitch { ch: *ch };
                    dev.surface.set(key, OutVal::U14(p), &mut outs[d]);
                }
            }
            for (cc, r) in &dev.map.relative {
                if let Some(x) = r.exec.as_ref().filter(|x| x.exec == exec) {
                    dev.rel_value.insert(*cc, v.round() as u8);
                    if let Some(ring) = &x.ring {
                        let rv = map_range(v, 0.0, 127.0, ring.from as f64, ring.to as f64);
                        dev.surface.set(OutKey::Cc { ch: 1, cc: ring.cc }, OutVal::U7(rv), &mut outs[d]);
                    }
                }
            }
        }
    }

    fn button_feedback(&mut self, exec: Exec, on: bool, outs: &mut [Vec<Vec<u8>>]) {
        for d in 0..self.devices.len() {
            let dev = &self.devices[d];
            let hits: Vec<(u8, u8, Option<u8>, Option<Feedback>)> = dev
                .map
                .notes
                .iter()
                .filter(|(_, a)| a.exec == Some(exec))
                .map(|(n, a)| (*n, a.channel.unwrap_or(dev.map.default_channel), a.permanent_feedback, a.feedback))
                .collect();
            for (note, ch, pf, fb) in hits {
                let led = match pf {
                    Some(v) => Led::Raw(v),
                    None if on => Led::On,
                    None => Led::Off,
                };
                self.led(d, note, ch, led, fb, &mut outs[d]);
            }
        }
    }

    fn master_feedback(&mut self, master: &str, on: bool, outs: &mut [Vec<Vec<u8>>]) {
        for d in 0..self.devices.len() {
            let dev = &self.devices[d];
            let hits: Vec<(u8, u8, bool, Option<Feedback>)> = dev
                .map
                .notes
                .iter()
                .filter(|(_, a)| a.cmd.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(master)))
                .map(|(n, a)| (*n, a.channel.unwrap_or(dev.map.default_channel), a.permanent_feedback.is_some(), a.feedback))
                .collect();
            for (note, ch, pf, fb) in hits {
                self.led(d, note, ch, if pf || on { Led::On } else { Led::Off }, fb, &mut outs[d]);
            }
        }
    }

    fn color_feedback(&mut self, exec: Exec, rgba: &str, outs: &mut [Vec<Vec<u8>>]) {
        let color = xtouch::nearest_color(rgba);
        for d in 0..self.devices.len() {
            let strips: Vec<u8> = self.devices[d].map.display.iter().filter(|(_, e)| **e == exec).map(|(s, _)| *s).collect();
            if strips.is_empty() {
                continue;
            }
            for s in strips {
                self.devices[d].colors[s as usize] = color;
            }
            self.render_display(d, &mut outs[d]);
        }
    }

    fn name_feedback(&mut self, exec: Exec, value: &str, outs: &mut [Vec<Vec<u8>>]) {
        let mut parts = value.splitn(2, ';');
        let seq = xtouch::strip_text(parts.next().unwrap_or(""));
        let cue = xtouch::strip_text(parts.next().unwrap_or(""));
        for d in 0..self.devices.len() {
            let strips: Vec<u8> = self.devices[d].map.display.iter().filter(|(_, e)| **e == exec).map(|(s, _)| *s).collect();
            if strips.is_empty() {
                continue;
            }
            for s in strips {
                self.devices[d].names[s as usize] = (seq, cue);
            }
            self.render_display(d, &mut outs[d]);
        }
    }

    /// Write names/colors (or the link-lost overlay) into the surface model.
    fn render_display(&mut self, d: usize, out: &mut Vec<Vec<u8>>) {
        let dev = &mut self.devices[d];
        if !dev.map.mc_mode || dev.map.display.is_empty() {
            return;
        }
        let lost = self.ma_lost;
        let mut colors = [xtouch::COLOR_OFF; 8];
        for strip in 0..8u8 {
            let (top, bottom, color) = if lost {
                (xtouch::strip_text("MA LINK"), xtouch::strip_text(" LOST"), xtouch::COLOR_RED)
            } else if dev.map.display.contains_key(&strip) {
                let (t, b) = dev.names[strip as usize];
                (t, b, dev.colors[strip as usize])
            } else {
                (BLANK, BLANK, xtouch::COLOR_OFF)
            };
            colors[strip as usize] = color;
            dev.surface.set(OutKey::LcdText { offset: xtouch::lcd_offset(strip, 0) }, OutVal::Text(top), out);
            dev.surface.set(OutKey::LcdText { offset: xtouch::lcd_offset(strip, 1) }, OutVal::Text(bottom), out);
        }
        dev.surface.set(OutKey::LcdColors, OutVal::Colors(colors), out);
    }

    // ------------------------------------------------------------------ timecode

    fn timecode_feedback(&mut self, slot_no: u8, time: &str, outs: &mut [Vec<Vec<u8>>]) {
        // MA timestring like "1h02m03:15", "02m03:15" or "03:15"
        let h = time.find('h');
        let m = time.find('m');
        let s = time.find(':');
        let hrs = h.map(|i| &time[..i]).unwrap_or("0");
        let mins = match m {
            Some(mi) => &time[h.map(|i| i + 1).unwrap_or(0).min(mi)..mi],
            None => "0",
        };
        let sec_start = m.map(|i| i + 1).unwrap_or(0);
        let (secs, mili) = match s {
            Some(si) if si >= sec_start => (&time[sec_start..si], &time[si + 1..]),
            _ => (&time[sec_start..], ""),
        };
        let slot = self.tc_slots.entry(slot_no).or_default();
        slot.hrs = hrs.to_string();
        slot.mins = mins.to_string();
        slot.secs = secs.to_string();
        slot.mili = mili.to_string();
        if self.tc_selected == slot_no {
            let s = slot.clone();
            self.timecode_show(&s, outs);
        }
    }

    fn timecode_show(&mut self, s: &Slot, outs: &mut [Vec<Vec<u8>>]) {
        let fields = [(9u8, pad_end(&s.mili, 2)), (7, pad_start(&s.secs, 2)), (5, pad_start(&s.mins, 2)), (2, pad_start(&s.hrs, 3))];
        for (d, dev) in self.devices.iter_mut().enumerate() {
            if !dev.map.mc_mode || !dev.map.timecode_display {
                continue;
            }
            for (start, text) in &fields {
                for (i, b) in text.bytes().enumerate() {
                    let cc = xtouch::segment_cc(start + i as u8);
                    dev.surface.set(OutKey::Cc { ch: 1, cc }, OutVal::U7(b & 0x7F), &mut outs[d]);
                }
            }
        }
    }

    fn timecode_reset_all(&mut self, outs: &mut [Vec<Vec<u8>>]) {
        let sel = b'0' + self.tc_selected;
        for (d, dev) in self.devices.iter_mut().enumerate() {
            if !dev.map.mc_mode || !dev.map.timecode_display {
                continue;
            }
            for seg in 0..12u8 {
                dev.surface.set(OutKey::Cc { ch: 1, cc: 75 - seg }, OutVal::U7(0), &mut outs[d]);
            }
            dev.surface.set(OutKey::Cc { ch: 1, cc: xtouch::segment_cc(1) }, OutVal::U7(sel), &mut outs[d]);
        }
    }

    // ------------------------------------------------------------------ helpers

    fn attribute_leds(&mut self, outs: &mut [Vec<Vec<u8>>]) {
        for d in 0..self.devices.len() {
            let hits: Vec<(u8, bool)> = self.devices[d]
                .map
                .notes
                .iter()
                .filter_map(|(n, a)| a.attribute.as_ref().map(|at| (*n, at.eq_ignore_ascii_case(&self.current_attribute))))
                .collect();
            for (note, on) in hits {
                self.led(d, note, 1, if on { Led::On } else { Led::Off }, None, &mut outs[d]);
            }
        }
    }

    fn led(&mut self, d: usize, note: u8, ch: u8, led: Led, fb: Option<Feedback>, out: &mut Vec<Vec<u8>>) {
        let dev = &mut self.devices[d];
        let fb = fb.unwrap_or(dev.map.feedback);
        // Mackie Control LEDs live on channel 1; "off" is note-on with velocity 0.
        let ch = if dev.map.mc_mode { 1 } else { ch };
        let vel = match led {
            Led::On => fb.on,
            Led::Off if dev.map.mc_mode => 0,
            Led::Off => fb.off,
            Led::Raw(v) => v,
        };
        dev.surface.set(OutKey::Note { ch, note }, OutVal::U7(vel), out);
    }

    /// The page an executor is on: the current page unless it is fixed elsewhere.
    fn page_of(&self, exec: Exec) -> &str {
        self.exec_page.get(&exec).map(String::as_str).unwrap_or(&self.page)
    }

    fn send_fader(&self, sink: &mut dyn Sink, exec: Exec, v: i32) {
        let range = self.cfg.fader_range.max(1) as f64;
        let scaled = (v.clamp(0, 127) as f64 * range / 127.0).round() as i32;
        self.osc_int(sink, &format!("/Page{}/Fader{}", self.page_of(exec), exec), scaled);
    }

    fn osc_int(&self, sink: &mut dyn Sink, path: &str, v: i32) {
        let addr = format!("{}{}", self.cfg.prefix, path);
        debug!(%addr, v, "OSC out");
        sink.osc(&addr, vec![OscType::Int(v)]);
    }

    fn send_cmd(&self, sink: &mut dyn Sink, cmd: &str) {
        let addr = format!("{}/cmd", self.cfg.prefix);
        debug!(%addr, cmd, "OSC out");
        sink.osc(&addr, vec![OscType::String(cmd.to_string())]);
    }

    fn request_reload(&mut self, sink: &mut dyn Sink, now: Instant) {
        if self.last_reload_request.is_some_and(|t| now.duration_since(t) < RELOAD_MIN_INTERVAL) {
            return;
        }
        self.last_reload_request = Some(now);
        self.send_cmd(sink, "Lua 'SetVar(GlobalVars(), \"forceReload\", true)'");
    }

    fn outs(&self) -> Vec<Vec<Vec<u8>>> {
        vec![Vec::new(); self.devices.len()]
    }

    fn flush(&self, outs: Vec<Vec<Vec<u8>>>, sink: &mut dyn Sink) {
        for (d, msgs) in outs.into_iter().enumerate() {
            for m in msgs {
                sink.midi(d, &m);
            }
        }
    }
}

fn as_u7(v: &OutVal) -> Option<u8> {
    match v {
        OutVal::U7(v) => Some(*v),
        OutVal::U14(p) => Some(((*p as f64 / 16383.0) * 127.0).round() as u8),
        _ => None,
    }
}

fn fader_keys(map: &DeviceMapping, exec: Exec) -> Vec<OutKey> {
    let mut keys: Vec<OutKey> =
        map.pitch.iter().filter(|(_, e)| **e == exec).map(|(ch, _)| OutKey::Pitch { ch: *ch }).collect();
    keys.extend(map.control.iter().filter(|(_, e)| **e == exec).map(|(cc, _)| OutKey::Cc { ch: 1, cc: *cc }));
    keys
}

fn map_range(v: f64, from_lo: f64, from_hi: f64, to_lo: f64, to_hi: f64) -> u8 {
    let pct = (v - from_lo) / (from_hi - from_lo);
    let r = pct * (to_hi - to_lo) + to_lo;
    r.clamp(to_lo.min(to_hi), to_hi.max(to_lo)).round() as u8
}

/// "Fader201" -> ("Fader", Some(201)); "Timecode3" -> ("Timecode", Some(3)).
fn split_trailing_number(s: &str) -> (&str, Option<u16>) {
    let idx = s.rfind(|c: char| !c.is_ascii_digit()).map(|i| i + 1).unwrap_or(0);
    (&s[..idx], s[idx..].parse().ok())
}

fn pad_start(s: &str, n: usize) -> String {
    let p = format!("{s:0>n$}");
    p[p.len() - n..].to_string()
}

fn pad_end(s: &str, n: usize) -> String {
    let p = format!("{s:0<n$}");
    p[..n].to_string()
}

fn arg_f64(a: &OscType) -> Option<f64> {
    match a {
        OscType::Int(i) => Some(*i as f64),
        OscType::Long(i) => Some(*i as f64),
        OscType::Float(f) => Some(*f as f64),
        OscType::Double(f) => Some(*f),
        OscType::String(s) => s.trim().parse().ok(),
        OscType::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
    .filter(|v: &f64| v.is_finite())
}

fn arg_str(a: &OscType) -> Option<String> {
    match a {
        OscType::String(s) => Some(s.clone()),
        OscType::Int(i) => Some(i.to_string()),
        OscType::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{load_file, parse_legacy};
    use std::path::Path;

    #[derive(Default)]
    struct Rec {
        midi: Vec<(usize, Vec<u8>)>,
        osc: Vec<(String, Vec<OscType>)>,
    }
    impl Sink for Rec {
        fn midi(&mut self, d: usize, b: &[u8]) {
            self.midi.push((d, b.to_vec()));
        }
        fn osc(&mut self, a: &str, args: Vec<OscType>) {
            self.osc.push((a.to_string(), args));
        }
    }

    fn mapping(name: &str) -> DeviceMapping {
        load_file(&Path::new(env!("CARGO_MANIFEST_DIR")).join("mappings").join(name)).unwrap()
    }

    /// Fader range 127: outgoing OSC equals the raw 0..127 surface value.
    fn setup(maps: Vec<DeviceMapping>) -> (Engine, Rec, Instant) {
        setup_with(maps, EngineConfig { fader_range: 127, ..EngineConfig::default() })
    }

    fn setup_with(maps: Vec<DeviceMapping>, cfg: EngineConfig) -> (Engine, Rec, Instant) {
        let t0 = Instant::now();
        let devs = maps.into_iter().enumerate().map(|(i, m)| (format!("dev{i}"), m)).collect();
        let mut e = Engine::new(cfg, devs, t0);
        let mut r = Rec::default();
        e.start(&mut r, t0);
        for i in 0..e.device_count() {
            e.device_connected(i, &mut r, t0);
        }
        r.midi.clear();
        r.osc.clear();
        (e, r, t0)
    }

    fn s(v: &str) -> OscType {
        OscType::String(v.into())
    }

    #[test]
    fn xtouch_fader_moves_ma_fader() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.midi_in(0, &MidiMsg::Pitch { ch: 1, val: 16383 }.encode(), &mut r, t0);
        assert_eq!(r.osc, vec![("/Page1/Fader201".to_string(), vec![OscType::Int(127)])]);
    }

    #[test]
    fn fader_values_are_scaled_to_ma_fader_range() {
        // MA's default FaderRange is 100: full travel must arrive as 100, not 127
        let (mut e, mut r, t0) = setup_with(vec![mapping("xTouch1.json")], EngineConfig::default());
        e.midi_in(0, &MidiMsg::Pitch { ch: 1, val: 16383 }.encode(), &mut r, t0);
        e.midi_in(0, &MidiMsg::Pitch { ch: 2, val: 8192 }.encode(), &mut r, t0);
        e.midi_in(0, &MidiMsg::Pitch { ch: 3, val: 0 }.encode(), &mut r, t0);
        let sent: Vec<(String, Vec<OscType>)> = r.osc.clone();
        assert_eq!(
            sent,
            vec![
                ("/Page1/Fader201".to_string(), vec![OscType::Int(100)]),
                ("/Page1/Fader202".to_string(), vec![OscType::Int(50)]),
                ("/Page1/Fader203".to_string(), vec![OscType::Int(0)]),
            ]
        );
    }

    #[test]
    fn fader_filter_hysteresis() {
        let f = |st: Option<FaderIn>, v| fader_filter(st, v);
        let s = |last, dir| Some(FaderIn { last, dir });
        assert_eq!(f(None, 64), s(64, 0));
        assert_eq!(f(s(64, 1), 64), None, "repeat");
        assert_eq!(f(s(64, 1), 65), s(65, 1), "same direction, one step");
        assert_eq!(f(s(64, 1), 63), None, "one-step reversal");
        assert_eq!(f(s(64, 1), 62), s(62, -1), "two-step reversal");
        assert_eq!(f(s(64, 0), 65), None, "one step from rest");
        assert_eq!(f(s(1, 1), 0), s(0, -1), "end always reachable");
        assert_eq!(f(s(126, -1), 127), s(127, 1), "end always reachable");
    }

    #[test]
    fn wobbling_compact_fader_is_not_forwarded() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouchCompact1.json")]);
        let mut t = t0;
        // operator moves fader 7 up to 64, then lets go; the fader wobbles 63/64
        for v in [60, 61, 62, 63, 64, 63, 64, 63, 64, 63, 64, 63] {
            t += Duration::from_millis(20);
            e.midi_in(0, &MidiMsg::Cc { ch: 1, cc: 7, val: v }.encode(), &mut r, t);
            e.tick(&mut r, t);
        }
        let sent: Vec<i32> = r.osc.iter().map(|(_, a)| if let OscType::Int(i) = a[0] { i } else { -1 }).collect();
        assert_eq!(sent, vec![60, 62, 63, 64], "first one-step change from rest is skipped");

        // MA moves the motor to 100; settling wobble around it is ignored too
        r.osc.clear();
        t += Duration::from_secs(1);
        e.tick(&mut r, t);
        e.osc_in("/Page1/Fader207", &[OscType::Int(100)], &mut r, t);
        for v in [101, 99, 100, 101, 100] {
            e.midi_in(0, &MidiMsg::Cc { ch: 1, cc: 7, val: v }.encode(), &mut r, t);
        }
        assert!(r.osc.is_empty(), "{:?}", r.osc);
        // a real move still goes through once the motor has arrived
        t += MOTOR_TRAVEL;
        e.midi_in(0, &MidiMsg::Cc { ch: 1, cc: 7, val: 90 }.encode(), &mut r, t);
        assert_eq!(r.osc, vec![("/Page1/Fader207".to_string(), vec![OscType::Int(90)])]);
    }

    #[test]
    fn fast_ma_swipe_motor_travel_is_not_echoed_back() {
        // The Compact has no touch sensing and reports its fader position while
        // the motor travels. Those reports must not be treated as hand moves.
        let (mut e, mut r, t0) = setup(vec![mapping("xTouchCompact1.json")]);
        let mut t = t0;
        let cc7 = |v| MidiMsg::Cc { ch: 1, cc: 7, val: v }.encode();
        e.osc_in("/Page1/Fader207", &[OscType::Int(0)], &mut r, t); // reload: fader at 0
        t += Duration::from_secs(1);
        e.osc_in("/Page1/Fader207", &[OscType::Int(50)], &mut r, t); // mid-swipe poll
        for v in [5, 15, 25] {
            t += Duration::from_millis(20);
            e.midi_in(0, &cc7(v), &mut r, t);
            e.tick(&mut r, t);
        }
        t += Duration::from_millis(40);
        e.osc_in("/Page1/Fader207", &[OscType::Int(127)], &mut r, t); // swipe ends
        e.tick(&mut r, t);
        for v in [40, 60, 90, 120, 127] {
            t += Duration::from_millis(20);
            e.midi_in(0, &cc7(v), &mut r, t);
            e.tick(&mut r, t);
        }
        assert!(r.osc.is_empty(), "motor travel was sent to MA: {:?}", r.osc);
        e.tick(&mut r, t + Duration::from_secs(1));
        assert_eq!(r.midi.last(), Some(&(0, cc7(127))), "motor must end at MA's value");

        // after the motor settled, a hand move is forwarded again
        r.osc.clear();
        t += Duration::from_secs(1);
        e.midi_in(0, &cc7(100), &mut r, t);
        assert_eq!(r.osc, vec![("/Page1/Fader207".to_string(), vec![OscType::Int(100)])]);
    }

    #[test]
    fn ma_fader_moves_motor_unless_touched() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.osc_in("/Page1/Fader201", &[OscType::Int(127)], &mut r, t0);
        assert_eq!(r.midi, vec![(0, MidiMsg::Pitch { ch: 1, val: 16383 }.encode())]);
        r.midi.clear();

        // touch fader 1 (note 104), MA sends a value: motor must not move
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 104, vel: 127 }.encode(), &mut r, t0);
        e.osc_in("/Page1/Fader201", &[OscType::Int(0)], &mut r, t0);
        e.tick(&mut r, t0 + Duration::from_secs(5));
        assert!(r.midi.is_empty(), "{:?}", r.midi);

        // release: after the echo guard the latest MA value is applied
        let t1 = t0 + Duration::from_secs(6);
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 104, vel: 0 }.encode(), &mut r, t1);
        e.tick(&mut r, t1 + Duration::from_millis(350));
        assert_eq!(r.midi, vec![(0, MidiMsg::Pitch { ch: 1, val: 0 }.encode())]);
    }

    #[test]
    fn names_and_colors_go_to_scribble_strips() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.osc_in("/Page1/Name202", &[s("Café;Cue 3")], &mut r, t0);
        let top = xtouch::lcd_text_sysex(0x14, 7, b"Cafe   ");
        let bottom = xtouch::lcd_text_sysex(0x14, 63, b"Cue 3  ");
        assert!(r.midi.contains(&(0, top)), "{:?}", r.midi);
        assert!(r.midi.contains(&(0, bottom)));
        r.midi.clear();

        e.osc_in("/Page1/Color202", &[s("255;0;0;255")], &mut r, t0);
        let colors = r.midi.iter().find(|(_, b)| b[5] == 0x72).expect("color sysex");
        assert_eq!(colors.1[7], xtouch::COLOR_RED);
    }

    #[test]
    fn xtouch_with_extender() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json"), mapping("xTouchExtender1.json")]);
        e.osc_in("/Page1/Name210", &[s("Spots;Cue 1")], &mut r, t0);
        assert_eq!(r.midi, vec![
            (1, xtouch::lcd_text_sysex(0x15, 0, b"Spots  ")),
            (1, xtouch::lcd_text_sysex(0x15, 56, b"Cue 1  ")),
        ]);
        r.midi.clear();
        e.osc_in("/Page1/Color210", &[s("255;0;0;255")], &mut r, t0);
        assert!(r.midi.iter().all(|(d, b)| *d == 1 && b[4] == 0x15 && b[5] == 0x72), "{:?}", r.midi);

        // extender fader 1 is its own executor; touching it holds the motor
        r.osc.clear();
        e.midi_in(1, &MidiMsg::Pitch { ch: 1, val: 16383 }.encode(), &mut r, t0);
        assert_eq!(r.osc, vec![("/Page1/Fader210".to_string(), vec![OscType::Int(127)])]);
        r.midi.clear();
        e.midi_in(1, &MidiMsg::Note { ch: 1, note: 104, vel: 127 }.encode(), &mut r, t0);
        e.osc_in("/Page1/Fader210", &[OscType::Int(0)], &mut r, t0);
        e.tick(&mut r, t0 + Duration::from_secs(5));
        assert!(r.midi.is_empty(), "{:?}", r.midi);
    }

    #[test]
    fn compact_buttons_and_feedback() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouchCompact1.json")]);
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 0, vel: 127 }.encode(), &mut r, t0);
        assert_eq!(r.osc[0], ("/Page1/Key401".to_string(), vec![OscType::Int(127)]));

        e.osc_in("/Page1/Button401", &[s("On")], &mut r, t0);
        let fb = e.devices[0].map.notes[&0].feedback.unwrap_or(e.devices[0].map.feedback);
        assert!(r.midi.contains(&(0, MidiMsg::Note { ch: 1, note: 0, vel: fb.on }.encode())));
    }

    #[test]
    fn min_value_filters_release() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouchCompact1.json")]);
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 104, vel: 0 }.encode(), &mut r, t0);
        assert!(r.osc.is_empty());
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 104, vel: 127 }.encode(), &mut r, t0);
        assert_eq!(r.osc[0].1, vec![s("Previous Page")]);
    }

    #[test]
    fn page_change_is_followed() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.osc_in("/updatePage/current", &[OscType::Int(4)], &mut r, t0);
        e.midi_in(0, &MidiMsg::Pitch { ch: 2, val: 0 }.encode(), &mut r, t0);
        assert_eq!(r.osc.last().unwrap().0, "/Page4/Fader202");
    }

    #[test]
    fn fixed_executor_input_goes_to_its_page() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.osc_in("/updatePage/current", &[OscType::Int(2)], &mut r, t0);
        // 201 is fixed on page 1: the plugin reports it under /Page1 while on page 2
        e.osc_in("/Page1/Fader201", &[OscType::Int(127)], &mut r, t0);
        e.osc_in("/Page2/Fader202", &[OscType::Int(0)], &mut r, t0);
        assert!(r.midi.contains(&(0, MidiMsg::Pitch { ch: 1, val: 16383 }.encode())), "{:?}", r.midi);
        e.midi_in(0, &MidiMsg::Pitch { ch: 1, val: 0 }.encode(), &mut r, t0 + Duration::from_secs(1));
        e.midi_in(0, &MidiMsg::Pitch { ch: 2, val: 8000 }.encode(), &mut r, t0 + Duration::from_secs(1));
        let addrs: Vec<&str> = r.osc.iter().map(|(a, _)| a.as_str()).collect();
        assert!(addrs.contains(&"/Page1/Fader201"), "{addrs:?}");
        assert!(addrs.contains(&"/Page2/Fader202"), "{addrs:?}");
        // after a page change the plugin re-reports; until then the current page is used
        e.osc_in("/updatePage/current", &[OscType::Int(3)], &mut r, t0);
        assert_eq!(e.page_of(201), "3");
    }

    #[test]
    fn executor_press_completes_pending_command_line() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.osc_in("/updatePage/current", &[OscType::Int(3)], &mut r, t0);
        e.osc_in("/cmdline", &[s("Assign Sequence 5")], &mut r, t0);
        r.osc.clear();
        // note 16 = SOLO row, executor 201
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 16, vel: 127 }.encode(), &mut r, t0);
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 16, vel: 0 }.encode(), &mut r, t0);
        assert_eq!(r.osc, vec![("/cmd".to_string(), vec![s(r#"SetGlobalVariable "ma3OscTarget" "Page 3.201""#)])]);

        // command line empty again: normal key press and release
        e.osc_in("/cmdline", &[s("")], &mut r, t0);
        r.osc.clear();
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 16, vel: 127 }.encode(), &mut r, t0);
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 16, vel: 0 }.encode(), &mut r, t0);
        assert_eq!(
            r.osc,
            vec![
                ("/Page3/Key201".to_string(), vec![OscType::Int(127)]),
                ("/Page3/Key201".to_string(), vec![OscType::Int(0)]),
            ]
        );
    }

    #[test]
    fn key_released_after_command_line_fills_still_releases() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 16, vel: 127 }.encode(), &mut r, t0);
        e.osc_in("/cmdline", &[s("Store")], &mut r, t0);
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 16, vel: 0 }.encode(), &mut r, t0);
        assert_eq!(r.osc.last().unwrap(), &("/Page1/Key201".to_string(), vec![OscType::Int(0)]));
    }

    #[test]
    fn ignore_cmdline_button_always_sends_key() {
        let mut m = mapping("xTouch1.json");
        m.notes.get_mut(&16).unwrap().ignore_cmdline = true;
        let (mut e, mut r, t0) = setup(vec![m]);
        e.osc_in("/cmdline", &[s("Assign Sequence 5")], &mut r, t0);
        r.osc.clear();
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 16, vel: 127 }.encode(), &mut r, t0);
        assert_eq!(r.osc, vec![("/Page1/Key201".to_string(), vec![OscType::Int(127)])]);
    }

    #[test]
    fn desk_lock_blocks_input() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.osc_in("/status/deskLocked", &[OscType::Bool(true)], &mut r, t0);
        e.midi_in(0, &MidiMsg::Pitch { ch: 1, val: 0 }.encode(), &mut r, t0);
        assert!(r.osc.is_empty());
        e.osc_in("/status/deskLocked", &[OscType::Bool(false)], &mut r, t0);
        e.midi_in(0, &MidiMsg::Pitch { ch: 1, val: 0 }.encode(), &mut r, t0);
        assert_eq!(r.osc.len(), 1);
    }

    #[test]
    fn reconnect_restores_everything() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.osc_in("/Page1/Fader203", &[OscType::Int(64)], &mut r, t0);
        e.osc_in("/Page1/Name203", &[s("Front;Open")], &mut r, t0);
        e.device_disconnected(0);
        e.osc_in("/Page1/Fader203", &[OscType::Int(100)], &mut r, t0);
        r.midi.clear();
        e.device_connected(0, &mut r, t0);
        let p = ((100.0 / 127.0) * 16383.0_f64).round() as u16;
        assert!(r.midi.contains(&(0, MidiMsg::Pitch { ch: 3, val: p }.encode())));
        assert!(r.midi.contains(&(0, xtouch::lcd_text_sysex(0x14, 14, b"Front  "))));
    }

    #[test]
    fn relative_encoder_out_of_range_is_ignored() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.midi_in(0, &MidiMsg::Cc { ch: 1, cc: 16, val: 3 }.encode(), &mut r, t0);
        assert!(r.osc.contains(&("/Page1/Fader401".into(), vec![OscType::Int(3)])));
        assert!(r.osc.contains(&("/Page1/Encoder401".into(), vec![OscType::Int(3)])));
        r.osc.clear();
        e.midi_in(0, &MidiMsg::Cc { ch: 1, cc: 16, val: 40 }.encode(), &mut r, t0);
        assert!(r.osc.is_empty());
    }

    #[test]
    fn attribute_encoder_respects_fine() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 48, vel: 127 }.encode(), &mut r, t0); // encoderFine
        e.midi_in(0, &MidiMsg::Cc { ch: 1, cc: 60, val: 65 }.encode(), &mut r, t0);
        assert_eq!(r.osc.last().unwrap().1, vec![s("Attribute dimmer at - 0.1")]);
    }

    #[test]
    fn timecode_play_pause_on_empty_slot_does_not_crash() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 52, vel: 127 }.encode(), &mut r, t0); // select slot 1
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 53, vel: 127 }.encode(), &mut r, t0); // long press
        e.tick(&mut r, t0 + Duration::from_secs(1));
        assert_eq!(r.osc.last().unwrap().1, vec![s("Off Timecodeslot 1")]);
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 53, vel: 0 }.encode(), &mut r, t0);
    }

    #[test]
    fn timecode_display() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.midi_in(0, &MidiMsg::Note { ch: 1, note: 52, vel: 127 }.encode(), &mut r, t0);
        r.midi.clear();
        e.osc_in("/Timecode1", &[s("1h02m03:15")], &mut r, t0);
        let segs: HashMap<u8, u8> = r
            .midi
            .iter()
            .filter_map(|(_, b)| match MidiMsg::parse(b) {
                Some(MidiMsg::Cc { cc, val, .. }) => Some((cc, val)),
                _ => None,
            })
            .collect();
        // hours "001" on segments 2-4, minutes "02" on 5-6, seconds "03" on 7-8, frames "15" on 9-10
        let digit = |seg: u8| segs.get(&xtouch::segment_cc(seg)).map(|v| *v as char);
        let shown: String = (2..=10).map(|s| digit(s).unwrap_or('_')).collect();
        assert_eq!(shown, "001020315");
    }

    #[test]
    fn heartbeat_loss_shows_on_strips() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json")]);
        e.tick(&mut r, t0 + Duration::from_secs(10));
        assert!(!e.ma_link_lost(), "no heartbeat seen yet: monitoring must stay off");
        e.osc_in("/pam/heartbeat", &[OscType::Int(1)], &mut r, t0);
        r.midi.clear();
        e.tick(&mut r, t0 + Duration::from_secs(4));
        assert!(e.ma_link_lost());
        assert!(r.midi.contains(&(0, xtouch::lcd_text_sysex(0x14, 0, b"MA LINK"))));
        e.osc_in("/pam/heartbeat", &[OscType::Int(1)], &mut r, t0 + Duration::from_secs(5));
        assert!(!e.ma_link_lost());
    }

    #[test]
    fn two_devices_get_independent_feedback() {
        let (mut e, mut r, t0) = setup(vec![mapping("xTouch1.json"), mapping("xTouchCompact1.json")]);
        e.osc_in("/Page1/Fader201", &[OscType::Int(127)], &mut r, t0);
        assert!(r.midi.contains(&(0, MidiMsg::Pitch { ch: 1, val: 16383 }.encode())));
        assert!(r.midi.contains(&(1, MidiMsg::Cc { ch: 1, cc: 1, val: 127 }.encode())));
    }

    #[test]
    fn unknown_device_index_is_ignored() {
        let (mut e, mut r, t0) = setup(vec![parse_legacy("{}").unwrap()]);
        e.midi_in(7, &[0x90, 1, 1], &mut r, t0);
        e.midi_in(0, &[0x90, 1, 1], &mut r, t0);
        e.osc_in("/Page1/Fader999", &[s("not a number")], &mut r, t0);
        e.osc_in("/Timecode", &[], &mut r, t0);
    }
}
