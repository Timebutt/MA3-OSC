//! The message monitor window (opened from the tray menu or with `--monitor`).
//! A native window with the system webview (WebKit on macOS, WebView2 on
//! Windows) showing the page in `monitor_view.html`.

use crate::monitor;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use tao::dpi::LogicalSize;
use tao::event_loop::EventLoopWindowTarget;
use tao::window::{Window, WindowBuilder, WindowId};
use tracing::{info, warn};
use wry::{WebView, WebViewBuilder};

const PAGE: &str = include_str!("monitor_view.html");
/// Max entries pushed to the page per refresh (keeps the window responsive).
const BATCH: usize = 500;

pub struct MonitorWindow {
    window: Window,
    webview: WebView,
    last_seq: u64,
    reply: Rc<RefCell<Option<String>>>,
}

impl MonitorWindow {
    pub fn open(target: &EventLoopWindowTarget<()>, save_dir: Option<PathBuf>) -> anyhow::Result<Self> {
        let window = WindowBuilder::new()
            .with_title("MA3-OSC message monitor")
            .with_inner_size(LogicalSize::new(980.0, 600.0))
            .with_min_inner_size(LogicalSize::new(420.0, 240.0))
            .build(target)?;
        let reply: Rc<RefCell<Option<String>>> = Rc::default();
        let r = reply.clone();
        let webview = WebViewBuilder::new()
            .with_html(PAGE)
            .with_ipc_handler(move |req| {
                let msg = save(req.body(), save_dir.as_deref());
                *r.borrow_mut() = Some(msg);
            })
            // the page is self-contained: never open popups
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .build(&window)?;
        window.set_focus();
        monitor::get().set_enabled(true);
        info!("message monitor opened");
        Ok(MonitorWindow { window, webview, last_seq: 0, reply })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn focus(&self) {
        self.window.set_visible(true);
        self.window.set_minimized(false);
        self.window.set_focus();
    }

    /// Push new captured messages to the page.
    pub fn pump(&mut self) {
        if let Some(msg) = self.reply.borrow_mut().take() {
            let _ = self.webview.evaluate_script(&format!("pb.saved({})", js_string(&msg)));
        }
        let (entries, dropped) = monitor::get().since(self.last_seq, BATCH);
        let Some(last) = entries.last() else { return };
        self.last_seq = last.seq;
        match serde_json::to_string(&entries) {
            Ok(json) => {
                if let Err(e) = self.webview.evaluate_script(&format!("pb.add({json},{dropped})")) {
                    warn!("monitor window: {e}");
                }
            }
            Err(e) => warn!("monitor window: {e}"),
        }
    }
}

impl Drop for MonitorWindow {
    fn drop(&mut self) {
        monitor::get().set_enabled(false);
        info!("message monitor closed");
    }
}

fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

#[derive(serde::Deserialize)]
struct SaveRequest {
    save: String,
    text: String,
}

/// Handle "Save…" from the page: write the visible lines to the log folder.
fn save(body: &str, dir: Option<&std::path::Path>) -> String {
    let Ok(req) = serde_json::from_str::<SaveRequest>(body) else {
        return "save failed: bad request".into();
    };
    let Some(dir) = dir else {
        return "save failed: no log folder configured".into();
    };
    let name: String = req.save.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(64).collect();
    let path = dir.join(format!("{name}.txt"));
    let result = std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, req.text));
    match result {
        Ok(()) => {
            info!("message monitor saved to {}", path.display());
            format!("saved to {}", path.display())
        }
        Err(e) => format!("save failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn save_stays_in_the_log_folder() {
        let dir = std::env::temp_dir().join(format!("ma3-osc-test-{}", std::process::id()));
        let msg = super::save(r#"{"save":"../../etc/passwd","text":"hi\n"}"#, Some(&dir));
        assert!(msg.starts_with("saved to"), "{msg}");
        assert_eq!(std::fs::read_to_string(dir.join("etcpasswd.txt")).unwrap(), "hi\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
