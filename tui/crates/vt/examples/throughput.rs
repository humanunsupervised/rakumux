//! Measure emulator parse throughput: throughput FILE [ROWS COLS SCROLLBACK]
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).unwrap();
    let rows = args.get(2).map_or(40, |v| v.parse().unwrap());
    let cols = args.get(3).map_or(120, |v| v.parse().unwrap());
    let scrollback = args.get(4).map_or(2000, |v| v.parse().unwrap());
    let mut emu = vt::Emulator::responding(rows, cols, scrollback, vt::Colors::default());
    let start = Instant::now();
    for chunk in data.chunks(8192) {
        emu.feed(chunk);
    }
    let secs = start.elapsed().as_secs_f64();
    println!(
        "{:.1} MB in {:.2}s = {:.1} MB/s",
        data.len() as f64 / 1e6,
        secs,
        data.len() as f64 / 1e6 / secs
    );
}
