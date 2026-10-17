//! Emulator fidelity spike: run a program in a PTY through `vt::Emulator`,
//! play a scripted sequence of inputs, then print the screen.
//!
//! Usage: spike COLS ROWS STEPS -- PROGRAM [ARGS...]
//! STEPS is `|`-separated: `w:MS` wait, `t:TEXT` type text (with \r \e \t
//! escapes), `x:HEX` raw bytes, `r:ROWSxCOLS` resize.
//! Prints the final screen, then a `--- queries ---` section listing the
//! terminal queries the program sent and whether they were answered.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use vt::{Colors, Emulator};

fn unescape(text: &str) -> Vec<u8> {
    text.replace("\\r", "\r")
        .replace("\\e", "\x1b")
        .replace("\\t", "\t")
        .replace("\\n", "\n")
        .into_bytes()
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// Find query-like escape sequences in program output.
fn queries(bytes: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let text = String::from_utf8_lossy(bytes);
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == '\x1b' && chars[i + 1] == '[' {
            let start = i;
            let mut j = i + 2;
            while j < chars.len() && !('@'..='~').contains(&chars[j]) {
                j += 1;
            }
            if j < chars.len() {
                let seq: String = chars[start + 2..=j].iter().collect();
                let fin = chars[j];
                let is_query = matches!(fin, 'n' | 'c' | 'q' | 'u' | 't')
                    && (seq.starts_with('?')
                        || seq.starts_with('>')
                        || seq.starts_with('=')
                        || fin == 'n'
                        || fin == 'c'
                        || seq.starts_with("14")
                        || seq.starts_with("18")
                        || seq.starts_with("16"))
                    || seq.ends_with("$p");
                if is_query && !seq.ends_with('h') {
                    found.push(format!("CSI {seq}"));
                }
            }
            i = j;
        } else if chars[i] == '\x1b' && chars[i + 1] == ']' {
            let mut j = i + 2;
            while j < chars.len() && chars[j] != '\x07' && !(chars[j] == '\x1b') {
                j += 1;
            }
            let seq: String = chars[i + 2..j.min(chars.len())].iter().collect();
            if seq.ends_with('?') {
                found.push(format!("OSC {seq}"));
            }
            i = j;
        } else if chars[i] == '\x1b' && chars[i + 1] == 'P' {
            let mut j = i + 2;
            while j + 1 < chars.len() && !(chars[j] == '\x1b' && chars[j + 1] == '\\') {
                j += 1;
            }
            let seq: String = chars[i + 2..j.min(chars.len())].iter().collect();
            found.push(format!("DCS {seq}"));
            i = j;
        } else {
            i += 1;
        }
    }
    found
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sep = args.iter().position(|a| a == "--").expect("missing --");
    let cols: u16 = args[1].parse().unwrap();
    let rows: u16 = args[2].parse().unwrap();
    let steps = args[3].clone();
    let program = &args[sep + 1..];

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(&program[0]);
    cmd.args(&program[1..]);
    cmd.cwd(std::env::current_dir().unwrap());
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut emu = Emulator::responding(rows, cols, 1000, Colors::default());
    let mut seen_queries: Vec<String> = Vec::new();
    let mut replies_sent = 0usize;
    let mut pump = |emu: &mut Emulator, writer: &mut Box<dyn Write + Send>, until: Instant| {
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left.min(Duration::from_millis(20))) {
                Ok(bytes) => {
                    seen_queries.extend(queries(&bytes));
                    let out = emu.feed(&bytes);
                    if !out.replies.is_empty() {
                        replies_sent += 1;
                        writer.write_all(&out.replies).unwrap();
                        writer.flush().unwrap();
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    };

    for step in steps.split('|').filter(|s| !s.is_empty()) {
        let (kind, value) = step.split_at(2);
        match kind {
            "w:" => {
                let until = Instant::now() + Duration::from_millis(value.parse().unwrap());
                pump(&mut emu, &mut writer, until);
            }
            "t:" => {
                writer.write_all(&unescape(value)).unwrap();
                writer.flush().unwrap();
            }
            "x:" => {
                writer.write_all(&hex(value)).unwrap();
                writer.flush().unwrap();
            }
            "r:" => {
                let (r, c) = value.split_once('x').unwrap();
                let (r, c): (u16, u16) = (r.parse().unwrap(), c.parse().unwrap());
                pair.master
                    .resize(PtySize {
                        rows: r,
                        cols: c,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .unwrap();
                emu.resize(r, c);
            }
            other => panic!("unknown step {other}"),
        }
    }
    pump(
        &mut emu,
        &mut writer,
        Instant::now() + Duration::from_millis(300),
    );

    println!("{}", emu.text());
    println!("--- queries ({replies_sent} reply batches) ---");
    seen_queries.sort();
    seen_queries.dedup();
    for q in seen_queries {
        println!("{}", q.escape_debug());
    }
    let _ = child.kill();
}
