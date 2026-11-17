//! Menu-bar (macOS) / notification-area (Windows) icon.
//!
//! The icon is a grandMA3-style "3" badge whose status dot is green when grandMA3 and at least one surface are connected,
//! orange while MA or all surfaces are missing, and red on a link loss or a start-up problem. The menu
//! shows the details. The bridge itself runs on its own thread: the tray only
//! reads the shared status, so a tray problem can never stop the bridge.

use crate::autostart;
use crate::monitor_view::MonitorWindow;
use crate::status::{self, Health, Shared};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use tracing::{info, warn};

pub struct Paths {
    pub config: PathBuf,
    pub log_dir: Option<PathBuf>,
    /// Open the message monitor right away (`--monitor`).
    pub open_monitor: bool,
}

const SIZE: u32 = 64;
/// Supersampling per axis, for anti-aliased edges.
const SS: u32 = 4;

/// Distance from `p` to the segment `a`–`b`.
fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * abx).powi(2) + (p.1 - a.1 - t * aby).powi(2)).sqrt()
}

/// Is `p` (in units of the big disc's radius, relative to its centre, y down)
/// on the geometric "3": a top bar, a diagonal, and a bowl that ends in a
/// sharp tail at the lower left.
fn on_three(p: (f32, f32)) -> bool {
    let (x, y) = (p.0 + 0.04, p.1); // optical centring
    const HALF: f32 = 0.095; // half the stroke width
    let bar = (-0.26..=0.34).contains(&x) && (-0.50 - HALF..=-0.50 + HALF).contains(&y);
    let diagonal = seg_dist((x, y), (0.26, -0.46), (0.0, -0.10)) <= HALF;
    let (dx, dy) = (x, y - 0.20);
    let a = dy.atan2(dx).to_degrees(); // -90 = up, 0 = right, 90 = down
    let bowl = ((dx * dx + dy * dy).sqrt() - 0.30).abs() <= HALF && (-90.0..=150.0).contains(&a);
    bar || diagonal || bowl
}

/// grandMA3-style badge: a white disc with a "3", and a status-coloured dot
/// at the top right, separated from the disc by a transparent gap.
fn pixels(health: Health) -> Vec<u8> {
    let dot = match health {
        Health::Good => [0x2e, 0xb8, 0x4b],
        Health::Warning => [0xf0, 0x9a, 0x1a],
        Health::Bad => [0xe0, 0x2b, 0x2b],
    };
    // Geometry in fractions of the icon size.
    let (main_c, main_r) = ((0.42, 0.58), 0.40);
    let (dot_c, dot_r) = ((0.77, 0.23), 0.215);
    let gap = 0.045;
    let dist = |p: (f32, f32), c: (f32, f32)| ((p.0 - c.0).powi(2) + (p.1 - c.1).powi(2)).sqrt();

    let sample = |p: (f32, f32)| -> [f32; 4] {
        let dd = dist(p, dot_c);
        if dd <= dot_r {
            // thin darker rim so the dot reads on any menu-bar colour
            let rim = dd > dot_r - 0.03;
            let k = if rim { 0.6 } else { 1.0 };
            return [dot[0] as f32 * k, dot[1] as f32 * k, dot[2] as f32 * k, 1.0];
        }
        let dm = dist(p, main_c);
        if dm <= main_r && dd > dot_r + gap {
            if on_three(((p.0 - main_c.0) / main_r, (p.1 - main_c.1) / main_r)) {
                return [0x1c as f32, 0x1c as f32, 0x1c as f32, 1.0];
            }
            // subtle grey edge keeps the white disc visible on a light menu bar
            let v = if dm > main_r - 0.025 { 0x9a } else { 0xf7 } as f32;
            return [v, v, v, 1.0];
        }
        [0.0; 4]
    };

    let mut px = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let n = (SS * SS) as f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut acc = [0.0f32; 4]; // premultiplied
            for sy in 0..SS {
                for sx in 0..SS {
                    let p = (
                        (x as f32 + (sx as f32 + 0.5) / SS as f32) / SIZE as f32,
                        (y as f32 + (sy as f32 + 0.5) / SS as f32) / SIZE as f32,
                    );
                    let s = sample(p);
                    for i in 0..3 {
                        acc[i] += s[i] * s[3];
                    }
                    acc[3] += s[3];
                }
            }
            let a = acc[3] / n;
            let un = |v: f32| if acc[3] > 0.0 { (v / acc[3]).round() as u8 } else { 0 };
            px.extend_from_slice(&[un(acc[0]), un(acc[1]), un(acc[2]), (a * 255.0).round() as u8]);
        }
    }
    px
}

fn icon(health: Health) -> Option<Icon> {
    Icon::from_rgba(pixels(health), SIZE, SIZE).map_err(|e| warn!("tray icon: {e}")).ok()
}

fn open_folder(p: &std::path::Path) {
    let _ = std::fs::create_dir_all(p);
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(p).spawn();
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(p).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let r = std::process::Command::new("xdg-open").arg(p).spawn();
    if let Err(e) = r {
        warn!("cannot open {}: {e}", p.display());
    }
}

struct Ui {
    tray: TrayIcon,
    ma: MenuItem,
    devices: Vec<MenuItem>,
    problem: MenuItem,
    autostart: CheckMenuItem,
    open_monitor: MenuItem,
    open_logs: MenuItem,
    open_config: MenuItem,
    quit: MenuItem,
    shown: Option<(Health, String)>,
}

fn build(st: &status::Status) -> anyhow::Result<Ui> {
    let menu = Menu::new();
    let ma = MenuItem::new(st.ma_text(), false, None);
    let devices: Vec<MenuItem> = st.devices.iter().map(|(n, _)| MenuItem::new(n, false, None)).collect();
    let problem = MenuItem::new("", false, None);
    let open_monitor = MenuItem::new("Message monitor…", true, None);
    let open_logs = MenuItem::new("Open log folder", true, None);
    let open_config = MenuItem::new("Open config folder", true, None);
    let autostart = CheckMenuItem::new("Start at login", true, autostart::is_installed(), None);
    let quit = MenuItem::new("Quit MA3-OSC", true, None);

    menu.append(&ma)?;
    for d in &devices {
        menu.append(d)?;
    }
    menu.append(&problem)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&open_monitor)?;
    menu.append(&open_logs)?;
    menu.append(&open_config)?;
    menu.append(&autostart)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;

    let mut b = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("MA3-OSC");
    if let Some(i) = icon(Health::Warning) {
        b = b.with_icon(i);
    }
    let tray = b.build()?;
    Ok(Ui { tray, ma, devices, problem, autostart, open_monitor, open_logs, open_config, quit, shown: None })
}

impl Ui {
    fn refresh(&mut self, st: &status::Status) {
        let summary = st.summary();
        let health = st.health();
        if self.shown.as_ref().is_some_and(|(h, s)| *h == health && *s == summary) {
            return;
        }
        if self.shown.as_ref().is_none_or(|(h, _)| *h != health) {
            let _ = self.tray.set_icon(icon(health));
        }
        let _ = self.tray.set_tooltip(Some(&summary));
        self.ma.set_text(st.ma_text());
        for (item, (name, up)) in self.devices.iter().zip(&st.devices) {
            item.set_text(format!("{name}: {}", if *up { "connected" } else { "not connected" }));
        }
        let problem: String = st.error.as_deref().unwrap_or("").chars().take(120).collect();
        self.problem.set_text(if problem.is_empty() { "No problems".to_string() } else { format!("Problem: {problem}") });
        self.shown = Some((health, summary));
    }
}

/// Runs the tray event loop on the calling (main) thread. Never returns.
/// `on_quit` runs before the process exits (flush logs etc.).
pub fn run(shared: Shared, paths: Paths, on_quit: impl FnOnce() + 'static) -> ! {
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::new().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory); // no Dock icon
    }

    let mut ui: Option<Ui> = None;
    let mut on_quit = Some(on_quit);
    let mut failed_once = false;
    let mut monitor: Option<MonitorWindow> = None;
    let mut want_monitor = paths.open_monitor;
    event_loop.run(move |event, target, control_flow| {
        let wait = if monitor.is_some() { 100 } else { 250 };
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(wait));

        if let Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } = &event
            && monitor.as_ref().is_some_and(|m| m.id() == *window_id)
        {
            monitor = None; // closing stops the capture
        }
        if want_monitor {
            want_monitor = false;
            match &monitor {
                Some(m) => m.focus(),
                None => match MonitorWindow::open(target, paths.log_dir.clone()) {
                    Ok(m) => monitor = Some(m),
                    Err(e) => warn!("cannot open the message monitor: {e}"),
                },
            }
        }
        if let Some(m) = monitor.as_mut() {
            m.pump();
        }

        // The tray must be created once the event loop is running (macOS).
        if matches!(event, Event::NewEvents(StartCause::Init)) || (ui.is_none() && !failed_once) {
            if ui.is_none() {
                match build(&status::snapshot(&shared)) {
                    Ok(u) => ui = Some(u),
                    Err(e) => {
                        failed_once = true;
                        warn!("cannot create the tray icon ({e}); the bridge keeps running without it");
                    }
                }
            }
        }
        let Some(ui) = ui.as_mut() else { return };
        ui.refresh(&status::snapshot(&shared));

        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if ev.id == *ui.quit.id() {
                info!("quit from the tray menu");
                if let Some(f) = on_quit.take() {
                    f();
                }
                *control_flow = ControlFlow::Exit;
            } else if ev.id == *ui.open_monitor.id() {
                want_monitor = true;
            } else if ev.id == *ui.open_logs.id() {
                if let Some(d) = &paths.log_dir {
                    open_folder(d);
                }
            } else if ev.id == *ui.open_config.id() {
                if let Some(d) = paths.config.parent() {
                    open_folder(d);
                }
            } else if ev.id == *ui.autostart.id() {
                let r = if autostart::is_installed() {
                    autostart::uninstall()
                } else {
                    autostart::current_exe().and_then(|exe| autostart::install(&exe, &paths.config))
                };
                match r {
                    Ok(msg) => info!("{msg}"),
                    Err(e) => warn!("autostart: {e:#}"),
                }
                ui.autostart.set_checked(autostart::is_installed());
            }
        }
    })
}
