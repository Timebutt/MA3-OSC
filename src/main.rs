//! MA3-OSC: Behringer X-Touch / X-Touch Compact <-> grandMA3 (OSC) bridge.
// On Windows, don't open a console window when started from Explorer or at login.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod app_bundle;
mod autostart;
mod config;
mod engine;
mod mapping;
mod mapping_v2;
mod mcp;
mod midi;
mod midi_io;
mod monitor;
mod monitor_view;
mod osc_io;
mod rpc;
mod status;
mod surface;
mod tray;
mod xtouch;

use anyhow::{Context, Result, bail};
use config::AppConfig;
use engine::{Engine, EngineConfig, Sink};
use rosc::OscType;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use status::Shared;
use tracing::{error, info};

pub enum Event {
    Midi { dev: usize, bytes: Vec<u8> },
    Osc { addr: String, args: Vec<OscType> },
}

const USAGE: &str = "\
MA3-OSC - X-Touch <-> grandMA3 OSC bridge

USAGE:
    ma3-osc [--config <file>]            run with a menu-bar / tray icon
    ma3-osc --monitor [--config <file>]  run and open the message monitor window
    ma3-osc --no-tray [--config <file>]  run without an icon (terminal / service)
    ma3-osc --check [--config <file>]    validate config and mappings, then exit
    ma3-osc --list-ports                 show the MIDI ports this computer sees
    ma3-osc --convert <old.json> [<new.json>]
                                            convert a pam-OSC mapping to the v2 format
                                            (prints to the terminal if no output file)
    ma3-osc --install-autostart [--config <file>]
    ma3-osc --uninstall-autostart        start MA3-OSC at login, or stop doing so

The default config is ./ma3-osc.json, else ma3-osc.json next to the executable,
else (macOS) ~/Library/Application Support/MA3-OSC/ma3-osc.json, which the
app bundle creates on its first start.
Set RUST_LOG=debug to log every MIDI/OSC message.";

#[derive(Default)]
struct Args {
    config: Option<PathBuf>,
    check: bool,
    list_ports: bool,
    no_tray: bool,
    monitor: bool,
    convert: Option<(PathBuf, Option<PathBuf>)>,
    install_autostart: bool,
    uninstall_autostart: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" | "-c" => a.config = Some(it.next().context("--config needs a file")?.into()),
            "--check" => a.check = true,
            "--list-ports" => a.list_ports = true,
            "--no-tray" => a.no_tray = true,
            "--monitor" => a.monitor = true,
            "--install-autostart" => a.install_autostart = true,
            "--uninstall-autostart" => a.uninstall_autostart = true,
            "--convert" => {
                let src = it.next().context("--convert needs a mapping file")?;
                let out = it.next_if(|n| !n.starts_with("--")).map(PathBuf::from);
                a.convert = Some((src.into(), out));
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            // macOS adds this when an app is started from Finder
            s if s.starts_with("-psn_") => {}
            other => bail!("unknown argument '{other}'\n\n{USAGE}"),
        }
    }
    Ok(a)
}

fn find_config(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(p) = explicit {
        return p;
    }
    let local = PathBuf::from("ma3-osc.json");
    if local.exists() {
        return local;
    }
    let exe = std::env::current_exe().ok();
    if let Some(p) = exe.as_ref().and_then(|e| e.parent()).map(|d| d.join("ma3-osc.json")).filter(|p| p.exists()) {
        return p;
    }
    #[cfg(target_os = "macos")]
    if let Some(p) = app_bundle::user_config(exe.as_deref()) {
        return p;
    }
    local
}

fn main() {
    #[cfg(windows)]
    attach_console();
    if let Err(e) = real_main() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// With the "windows" subsystem there is no console; reuse the one of the
/// terminal we were started from, so command-line output stays visible.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: plain Win32 call without pointers; failure (no parent console) is fine.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

type Devices = Vec<(String, mapping::DeviceMapping)>;

fn load_all(cfg_path: &Path) -> Result<(AppConfig, Devices)> {
    let cfg = AppConfig::load(cfg_path)?;
    let mut devices = Vec::new();
    for d in &cfg.devices {
        let map = mapping::load_file(&d.mapping).with_context(|| format!("device '{}'", d.name))?;
        devices.push((d.name.clone(), map));
    }
    Ok((cfg, devices))
}

fn convert(src: &Path, out: Option<&Path>) -> Result<()> {
    let m = mapping::load_file(src)?;
    let v2 = mapping_v2::to_v2(&m).with_context(|| format!("cannot convert {}", src.display()))?;
    let text = mapping_v2::to_json(&v2)?;
    // never write a file that would not load back identically
    let back = mapping_v2::parse_v2(&text).context("internal: converted mapping does not load")?;
    if back != m {
        bail!("internal: converted mapping differs from the original; please report this");
    }
    match out {
        Some(o) => {
            if o.exists() {
                bail!("{} already exists; choose another name or delete it first", o.display());
            }
            std::fs::write(o, text).with_context(|| format!("cannot write {}", o.display()))?;
            eprintln!("wrote {}", o.display());
        }
        None => print!("{text}"),
    }
    Ok(())
}

fn real_main() -> Result<()> {
    let args = parse_args()?;

    if args.list_ports {
        let (ins, outs) = midi_io::list_ports()?;
        println!("MIDI inputs:");
        ins.iter().for_each(|p| println!("    {p}"));
        println!("MIDI outputs:");
        outs.iter().for_each(|p| println!("    {p}"));
        return Ok(());
    }
    if let Some((src, out)) = &args.convert {
        return convert(src, out.as_deref());
    }
    if args.uninstall_autostart {
        println!("{}", autostart::uninstall()?);
        return Ok(());
    }

    let cfg_path = find_config(args.config);
    let cfg_path = std::path::absolute(&cfg_path).unwrap_or(cfg_path);

    if args.install_autostart {
        load_all(&cfg_path)?; // don't install something that can't start
        println!("{}", autostart::install(&autostart::current_exe()?, &cfg_path)?);
        return Ok(());
    }

    let loaded = load_all(&cfg_path);

    if args.check {
        let (cfg, devices) = loaded?;
        println!("config OK: {}", cfg_path.display());
        println!("  grandMA3: send to {}:{}, listen on {}", cfg.ma.host, cfg.ma.port, cfg.ma.listen_port);
        for ((name, map), d) in devices.iter().zip(&cfg.devices) {
            println!(
                "  {name}: port \"{}\" / \"{}\", {} ({} faders, {} buttons, {} encoders, {} strips)",
                d.input_match(),
                d.output_match(),
                match (map.mc_mode, map.extender) {
                    (true, false) => "X-Touch (Mackie Control)",
                    (true, true) => "X-Touch Extender (Mackie Control)",
                    _ => "standard MIDI",
                },
                map.pitch.len() + map.control.len(),
                map.notes.len(),
                map.relative.len(),
                map.display.len()
            );
        }
        return Ok(());
    }

    if args.no_tray {
        if args.monitor {
            bail!("--monitor needs the tray; use RUST_LOG=debug with --no-tray to log every message");
        }
        let (cfg, devices) = loaded?;
        let _log_guard = init_logging(cfg.log_dir.as_deref());
        start_message(&cfg_path);
        let shared = shared_status(&cfg);
        run_forever(cfg, devices, shared)
    }

    // Tray mode: a start-up problem is shown in the tray instead of exiting,
    // because there may be no terminal to print it to.
    let default_logs = cfg_path.parent().map(|d| d.join("logs"));
    match loaded {
        Ok((cfg, devices)) => {
            let log_guard = init_logging(cfg.log_dir.as_deref());
            start_message(&cfg_path);
            let shared = shared_status(&cfg);
            let paths = tray::Paths { config: cfg_path, log_dir: cfg.log_dir.clone(), open_monitor: args.monitor };
            let s = shared.clone();
            std::thread::Builder::new().name("bridge".into()).spawn(move || run_forever(cfg, devices, s))?;
            tray::run(shared, paths, move || drop(log_guard))
        }
        Err(e) => {
            let log_guard = init_logging(default_logs.as_deref());
            error!("cannot start: {e:#}");
            let shared = std::sync::Arc::new(std::sync::Mutex::new(status::Status::new(vec![])));
            status::update(&shared, |s| s.error = Some(format!("{e:#}")));
            tray::run(shared, tray::Paths { config: cfg_path, log_dir: default_logs, open_monitor: false }, move || drop(log_guard))
        }
    }
}

fn start_message(cfg_path: &Path) {
    std::panic::set_hook(Box::new(|info| error!("internal error (recovered): {info}")));
    info!(version = env!("CARGO_PKG_VERSION"), config = %cfg_path.display(), "MA3-OSC starting");
}

fn shared_status(cfg: &AppConfig) -> Shared {
    let names = cfg.devices.iter().map(|d| d.name.clone()).collect();
    std::sync::Arc::new(std::sync::Mutex::new(status::Status::new(names)))
}

/// Keeps the bridge running: if it cannot start (port in use, ...), the problem
/// is shown and it tries again every few seconds.
fn run_forever(cfg: AppConfig, devices: Devices, shared: Shared) -> ! {
    let retry = Duration::from_secs(5);
    let (tx, rx) = mpsc::channel::<Event>();
    let broker = cfg.mcp.as_ref().and_then(|m| match start_mcp(&cfg, m.port, &shared) {
        Ok(b) => Some(b),
        Err(e) => {
            // the surfaces still work; only agent access is missing
            error!("{e:#}");
            None
        }
    });
    loop {
        match osc_io::spawn_listener(cfg.ma.listen_port, tx.clone()) {
            Ok(()) => break,
            Err(e) => {
                error!("{e:#} (retrying every {retry:?})");
                status::update(&shared, |s| s.error = Some(format!("{e:#}")));
                std::thread::sleep(retry);
            }
        }
    }
    loop {
        let e = run(&cfg, devices.clone(), &rx, tx.clone(), &shared, broker.as_deref()).unwrap_err();
        error!("{e:#} (retrying in {retry:?})");
        status::update(&shared, |s| s.error = Some(format!("{e:#}")));
        std::thread::sleep(retry);
    }
}

/// The MCP server sends requests on its own socket; replies come back through
/// the main loop, which hands them to the broker.
fn start_mcp(cfg: &AppConfig, port: u16, shared: &Shared) -> Result<std::sync::Arc<rpc::Broker>> {
    let mut out = osc_io::OscOut::new(&cfg.ma.host, cfg.ma.port)?;
    let broker = std::sync::Arc::new(rpc::Broker::new(
        &cfg.ma.prefix,
        Duration::from_secs(2),
        Box::new(move |addr, args| out.send(addr, args)),
    ));
    let b = broker.clone();
    mcp::spawn(port, Box::new(move |tool, args| b.call(tool, args)), shared.clone())?;
    Ok(broker)
}

fn init_logging(dir: Option<&Path>) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    use std::io::IsTerminal;
    let stderr = fmt::layer().with_ansi(std::io::stderr().is_terminal()).with_writer(std::io::stderr);
    let file = dir.and_then(|d| {
        let _ = std::fs::create_dir_all(d);
        tracing_appender::rolling::RollingFileAppender::builder()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("ma3-osc")
            .filename_suffix("log")
            .max_log_files(14)
            .build(d)
            .map_err(|e| eprintln!("warning: file logging disabled ({}): {e}", d.display()))
            .ok()
    });
    match file {
        Some(appender) => {
            let (nb, guard) = tracing_appender::non_blocking(appender);
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr)
                .with(fmt::layer().with_ansi(false).with_writer(nb))
                .init();
            Some(guard)
        }
        None => {
            tracing_subscriber::registry().with(filter).with(stderr).init();
            None
        }
    }
}

struct IoSink<'a> {
    midi: &'a mut midi_io::MidiIo,
    osc: &'a mut osc_io::OscOut,
    names: &'a [String],
}

impl Sink for IoSink<'_> {
    fn midi(&mut self, device: usize, bytes: &[u8]) {
        let name = self.names.get(device).map_or("?", |n| n.as_str());
        monitor::get().midi(monitor::Dir::MidiOut, name, bytes);
        self.midi.send(device, bytes);
    }
    fn osc(&mut self, addr: &str, args: Vec<OscType>) {
        monitor::get().osc(monitor::Dir::OscOut, addr, &args);
        self.osc.send(addr, args);
    }
}

/// Run an engine call; a bug in one message must never take the bridge down.
fn guarded(what: &str, f: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        error!("recovered from internal error while handling {what}");
    }
}

/// Runs the bridge; only returns on a problem that needs a fresh start.
fn run(
    cfg: &AppConfig,
    devices: Devices,
    rx: &Receiver<Event>,
    tx: Sender<Event>,
    shared: &Shared,
    broker: Option<&rpc::Broker>,
) -> Result<()> {
    let mut osc = osc_io::OscOut::new(&cfg.ma.host, cfg.ma.port)?;
    let specs = cfg
        .devices
        .iter()
        .map(|d| midi_io::PortSpec { name: d.name.clone(), input: d.input_match().into(), output: d.output_match().into() })
        .collect();
    let mut midi = midi_io::MidiIo::new(specs, tx)?;
    info!("OSC: sending to {}:{}, listening on UDP {}", cfg.ma.host, cfg.ma.port, cfg.ma.listen_port);
    status::update(shared, |s| s.error = None);

    let ecfg = EngineConfig {
        prefix: cfg.ma.prefix.clone(),
        fader_range: cfg.ma.fader_range,
        echo_guard: Duration::from_millis(cfg.fader_echo_guard_ms),
        heartbeat_timeout: Duration::from_millis(cfg.heartbeat_timeout_ms),
        refresh_every: (cfg.surface_refresh_secs > 0).then(|| Duration::from_secs(cfg.surface_refresh_secs)),
    };
    let mut connected = vec![false; devices.len()];
    let names: Vec<String> = devices.iter().map(|(n, _)| n.clone()).collect();
    let mut engine = Engine::new(ecfg, devices, Instant::now());
    guarded("startup", || engine.start(&mut IoSink { midi: &mut midi, osc: &mut osc, names: &names }, Instant::now()));

    let scan_every = Duration::from_secs(1);
    let status_every = Duration::from_millis(200);
    let mut last_scan: Option<Instant> = None;
    let mut last_status: Option<Instant> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Event::Midi { dev, bytes }) => guarded("MIDI input", || {
                monitor::get().midi(monitor::Dir::MidiIn, names.get(dev).map_or("?", |n| n.as_str()), &bytes);
                engine.midi_in(dev, &bytes, &mut IoSink { midi: &mut midi, osc: &mut osc, names: &names }, Instant::now())
            }),
            Ok(Event::Osc { addr, args }) => guarded("OSC input", || {
                monitor::get().osc(monitor::Dir::OscIn, &addr, &args);
                // an agent reply; the engine still sees it as a sign of life from MA
                if let (rpc::REPLY_ADDR, Some(b)) = (addr.as_str(), broker) {
                    b.deliver(&args);
                }
                engine.osc_in(&addr, &args, &mut IoSink { midi: &mut midi, osc: &mut osc, names: &names }, Instant::now())
            }),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("internal event channel closed"),
        }

        let now = Instant::now();
        if last_scan.is_none_or(|t| now.duration_since(t) >= scan_every) {
            last_scan = Some(now);
            for (idx, up) in midi.scan() {
                if let Some(c) = connected.get_mut(idx) {
                    *c = up;
                }
                if up {
                    guarded("device connect", || {
                        engine.device_connected(idx, &mut IoSink { midi: &mut midi, osc: &mut osc, names: &names }, now)
                    });
                } else {
                    guarded("device disconnect", || engine.device_disconnected(idx));
                }
            }
        }
        guarded("tick", || engine.tick(&mut IoSink { midi: &mut midi, osc: &mut osc, names: &names }, now));

        if last_status.is_none_or(|t| now.duration_since(t) >= status_every) {
            last_status = Some(now);
            let ma = engine.ma_link();
            status::update(shared, |s| {
                s.ma = ma;
                for (d, up) in s.devices.iter_mut().zip(&connected) {
                    d.1 = *up;
                }
            });
        }
    }
}
