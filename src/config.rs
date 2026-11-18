//! Application config (`ma3-osc.json`). Strict: unknown keys are rejected so
//! a typo never silently falls back to a default during a show.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default)]
    pub ma: MaConfig,
    pub devices: Vec<DeviceConfig>,
    /// Re-send the complete surface state every N seconds (0 = never). Recovers
    /// a daisy-chained surface that was power-cycled without a USB change.
    #[serde(default = "default_refresh")]
    pub surface_refresh_secs: u64,
    /// After a fader is moved/released, ignore MA feedback for this long.
    #[serde(default = "default_echo_guard")]
    pub fader_echo_guard_ms: u64,
    /// Only active once the MA plugin sends /pam/heartbeat.
    #[serde(default = "default_heartbeat")]
    pub heartbeat_timeout_ms: u64,
    /// Log folder; default `logs` next to the config file.
    pub log_dir: Option<PathBuf>,
    /// Agent access (MCP server on 127.0.0.1); off when absent.
    pub mcp: Option<McpConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpConfig {
    /// TCP port of the MCP server, e.g. 9005.
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaConfig {
    /// Where grandMA3 onPC listens for OSC.
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_send_port")]
    pub port: u16,
    /// Where MA3-OSC listens for OSC from grandMA3.
    #[serde(default = "default_listen_port")]
    pub listen_port: u16,
    #[serde(default)]
    pub prefix: String,
    /// "FaderRange" of the OSC line in MA (MA's default is 100 = percent).
    #[serde(default = "default_fader_range")]
    pub fader_range: u16,
}

impl Default for MaConfig {
    fn default() -> Self {
        MaConfig { host: default_host(), port: default_send_port(), listen_port: default_listen_port(), prefix: String::new(), fader_range: default_fader_range() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceConfig {
    pub name: String,
    /// MIDI port name to look for (exact match preferred, else substring; case-insensitive).
    #[serde(rename = "match")]
    pub port_match: String,
    /// Override the match string for the input / output port only.
    pub input: Option<String>,
    pub output: Option<String>,
    /// Mapping JSON, relative to the config file.
    pub mapping: PathBuf,
}

fn default_refresh() -> u64 {
    10
}
fn default_echo_guard() -> u64 {
    300
}
fn default_heartbeat() -> u64 {
    3000
}
fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_fader_range() -> u16 {
    100
}
fn default_send_port() -> u16 {
    9003
}
fn default_listen_port() -> u16 {
    9004
}

impl AppConfig {
    /// Load and validate; mapping paths and log dir are made absolute.
    pub fn load(path: &Path) -> Result<AppConfig> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read config {}", path.display()))?;
        let mut cfg: AppConfig =
            serde_json::from_str(&text).with_context(|| format!("invalid config {}", path.display()))?;
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();

        if cfg.ma.fader_range == 0 {
            bail!("{}: \"ma.faderRange\" must be at least 1 (use MA's OSC FaderRange, default 100)", path.display());
        }
        if cfg.devices.is_empty() {
            bail!("{}: at least one device is required", path.display());
        }
        let mut names = HashSet::new();
        for d in &mut cfg.devices {
            if !names.insert(d.name.to_lowercase()) {
                bail!("{}: duplicate device name '{}'", path.display(), d.name);
            }
            if d.port_match.trim().is_empty() {
                bail!("{}: device '{}' has an empty \"match\"", path.display(), d.name);
            }
            if d.mapping.is_relative() {
                d.mapping = base.join(&d.mapping);
            }
        }
        cfg.log_dir = Some(match cfg.log_dir.take() {
            Some(p) if p.is_relative() => base.join(p),
            Some(p) => p,
            None => base.join("logs"),
        });
        Ok(cfg)
    }
}

impl DeviceConfig {
    pub fn input_match(&self) -> &str {
        self.input.as_deref().unwrap_or(&self.port_match)
    }
    pub fn output_match(&self) -> &str {
        self.output.as_deref().unwrap_or(&self.port_match)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_loads() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("ma3-osc.json");
        let cfg = AppConfig::load(&p).unwrap();
        assert_eq!(cfg.ma.port, 9003);
        for d in &cfg.devices {
            crate::mapping::load_file(&d.mapping).unwrap();
        }
    }

    #[test]
    fn typos_are_rejected() {
        let e = serde_json::from_str::<AppConfig>(r#"{"devices":[],"surfaceRefreshSec":5}"#).unwrap_err();
        assert!(e.to_string().contains("surfaceRefreshSec"), "{e}");
    }
}
