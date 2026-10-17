//! `rakumux-ptyd serve` runs the daemon in the foreground. Clients spawn it
//! detached (stdio on /dev/null, `setsid`); see `session_core::client`.

fn main() {
    std::process::exit(ptyd::server::main_entry(std::env::args().collect()));
}
