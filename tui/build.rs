use std::env;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Prefer externally supplied ORCHESTRA_VERSION, else Cargo package version
    let pkg_ver = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let ver = env::var("ORCHESTRA_VERSION").unwrap_or(pkg_ver);
    println!("cargo:rustc-env=ORCHESTRA_VERSION={}", ver);

    // Best-effort embed of git short SHA (optional)
    if let Ok(out) = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
    {
        if out.status.success() {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !sha.is_empty() {
                println!("cargo:rustc-env=ORCHESTRA_GIT_SHA={}", sha);
            }
        }
    }

    // Build timestamp (seconds since epoch)
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=ORCHESTRA_BUILD_DATE={}", ts);

    // Rebuild triggers
    println!("cargo:rerun-if-env-changed=ORCHESTRA_VERSION");
    println!("cargo:rerun-if-changed=build.rs");
}
