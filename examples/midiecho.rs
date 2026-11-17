use std::time::{Duration, Instant};
fn main() {
    let inp = midir::MidiInput::new("dump").unwrap();
    let out = midir::MidiOutput::new("probe").unwrap();
    let ip = inp.ports().into_iter().find(|p| inp.port_name(p).unwrap_or_default().to_lowercase().contains("x-touch")).unwrap();
    let op = out.ports().into_iter().find(|p| out.port_name(p).unwrap_or_default().to_lowercase().contains("x-touch")).unwrap();
    let t0 = Instant::now();
    let _c = inp.connect(&ip, "d", move |_, b, _| println!("in  {:7.3} {:02X?}", t0.elapsed().as_secs_f64(), b), ()).unwrap();
    let mut o = out.connect(&op, "p").unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    println!("--- idle above; sending CC9=64 (fader 9 motor)");
    o.send(&[0xB0, 9, 64]).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    println!("--- sending CC9=0");
    o.send(&[0xB0, 9, 0]).unwrap();
    std::thread::sleep(Duration::from_millis(1500));
}
