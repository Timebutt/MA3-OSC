use std::time::{Duration, Instant};
fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    let inp = midir::MidiInput::new("mididump").unwrap();
    let ports = inp.ports();
    let port = ports.iter().find(|p| inp.port_name(p).unwrap_or_default().to_lowercase().contains("x-touch")).expect("no X-Touch port");
    eprintln!("listening on {}", inp.port_name(port).unwrap());
    let t0 = Instant::now();
    let _c = inp.connect(port, "dump", move |_, b, _| println!("{:8.3} {:02X?}", t0.elapsed().as_secs_f64(), b), ()).unwrap();
    std::thread::sleep(Duration::from_secs(secs));
}
