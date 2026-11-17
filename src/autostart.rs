//! Start MA3-OSC at login.
//!
//! * macOS: a LaunchAgent in ~/Library/LaunchAgents. It also restarts the bridge
//!   if it ever crashes (but not after "Quit" from the menu).
//! * Windows: the "Run" key under HKEY_CURRENT_USER (no admin rights needed).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
const LABEL: &str = "org.ma3-osc.agent";

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(target_os = "macos")]
fn plist(exe: &Path, config: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>--config</string>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key>
    <dict><key>SuccessfulExit</key><false/></dict>
    <key>ThrottleInterval</key><integer>5</integer>
    <key>LimitLoadToSessionType</key><string>Aqua</string>
    <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
        xml_escape(&exe.to_string_lossy()),
        xml_escape(&config.to_string_lossy())
    )
}

#[cfg(target_os = "macos")]
pub fn install(exe: &Path, config: &Path) -> Result<String> {
    let p = plist_path()?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, plist(exe, config)).with_context(|| format!("cannot write {}", p.display()))?;
    Ok(format!("installed {} (starts at next login)", p.display()))
}

#[cfg(target_os = "macos")]
pub fn uninstall() -> Result<String> {
    let p = plist_path()?;
    if p.exists() {
        std::fs::remove_file(&p).with_context(|| format!("cannot remove {}", p.display()))?;
        Ok(format!("removed {}", p.display()))
    } else {
        Ok("autostart was not installed".into())
    }
}

#[cfg(target_os = "macos")]
pub fn is_installed() -> bool {
    plist_path().is_ok_and(|p| p.exists())
}

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const VALUE: &str = "ma3-osc";

#[cfg(windows)]
fn reg(args: &[&str]) -> Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output().context("cannot run reg.exe")
}

#[cfg(windows)]
pub fn install(exe: &Path, config: &Path) -> Result<String> {
    let cmd = format!("\"{}\" --config \"{}\"", exe.display(), config.display());
    let out = reg(&["add", RUN_KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &cmd, "/f"])?;
    if !out.status.success() {
        anyhow::bail!("reg add failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(format!("MA3-OSC will start at login ({RUN_KEY}\\{VALUE})"))
}

#[cfg(windows)]
pub fn uninstall() -> Result<String> {
    if !is_installed() {
        return Ok("autostart was not installed".into());
    }
    let out = reg(&["delete", RUN_KEY, "/v", VALUE, "/f"])?;
    if !out.status.success() {
        anyhow::bail!("reg delete failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok("autostart removed".into())
}

#[cfg(windows)]
pub fn is_installed() -> bool {
    reg(&["query", RUN_KEY, "/v", VALUE]).is_ok_and(|o| o.status.success())
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn install(_exe: &Path, _config: &Path) -> Result<String> {
    anyhow::bail!("autostart is only supported on macOS and Windows; use your desktop's autostart settings")
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn uninstall() -> Result<String> {
    anyhow::bail!("autostart is only supported on macOS and Windows")
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn is_installed() -> bool {
    false
}

/// The running executable, resolved to an absolute path.
pub fn current_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot find the MA3-OSC executable")?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn plist_escapes_paths() {
        let p = super::plist("/Apps/a&b/ma3-osc".as_ref(), "/x/<cfg>.json".as_ref());
        assert!(p.contains("<string>/Apps/a&amp;b/ma3-osc</string>"));
        assert!(p.contains("<string>/x/&lt;cfg&gt;.json</string>"));
    }
}
