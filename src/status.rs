//! Bridge status shared between the bridge thread and the tray icon.

use crate::engine::MaLink;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Status {
    pub ma: MaLink,
    /// (device name, connected)
    pub devices: Vec<(String, bool)>,
    /// A problem that stops the bridge (config error, port in use, ...).
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Good,
    Warning,
    Bad,
}

impl Status {
    pub fn new(devices: Vec<String>) -> Self {
        Status { ma: MaLink::Waiting, devices: devices.into_iter().map(|d| (d, false)).collect(), error: None }
    }

    pub fn health(&self) -> Health {
        if self.error.is_some() || self.ma == MaLink::Lost {
            Health::Bad
        } else if self.ma == MaLink::Ok && self.devices.iter().any(|(_, up)| *up) {
            Health::Good
        } else {
            Health::Warning
        }
    }

    pub fn ma_text(&self) -> &'static str {
        match self.ma {
            MaLink::Waiting => "grandMA3: waiting for data",
            MaLink::NoHeartbeat => "grandMA3: receiving (old plugin, no link check)",
            MaLink::Ok => "grandMA3: connected",
            MaLink::Lost => "grandMA3: LINK LOST",
        }
    }

    pub fn summary(&self) -> String {
        if let Some(e) = &self.error {
            return format!("MA3-OSC: {e}");
        }
        let up = self.devices.iter().filter(|(_, u)| *u).count();
        format!("MA3-OSC - {} - surfaces {up}/{}", self.ma_text(), self.devices.len())
    }
}

pub type Shared = Arc<Mutex<Status>>;

/// Update the shared status; a poisoned lock is recovered, never fatal.
pub fn update(s: &Shared, f: impl FnOnce(&mut Status)) {
    let mut g = s.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut g);
}

pub fn snapshot(s: &Shared) -> Status {
    s.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health() {
        let mut s = Status::new(vec!["a".into(), "b".into()]);
        assert_eq!(s.health(), Health::Warning);
        s.ma = MaLink::Ok;
        assert_eq!(s.health(), Health::Warning, "no surface connected");
        s.devices[0].1 = true;
        assert_eq!(s.health(), Health::Good, "one surface is enough");
        s.devices[1].1 = true;
        assert_eq!(s.health(), Health::Good);
        s.ma = MaLink::Lost;
        assert_eq!(s.health(), Health::Bad);
        s.ma = MaLink::Ok;
        s.error = Some("x".into());
        assert_eq!(s.health(), Health::Bad);
    }
}
