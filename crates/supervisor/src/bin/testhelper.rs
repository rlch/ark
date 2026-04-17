//! Test-only helper binary. Invoked by the unit tests in
//! `ark_supervisor::lock` and `ark_supervisor::daemon` via
//! `CARGO_BIN_EXE_ark-supervisor-testhelper`. It dispatches on `argv[1]`:
//!
//! - `lock-acquire-and-exit <base> <runtime> <config> <id>`
//!   Opens the lock, exits 0 on success, 2 on `AlreadyLocked`, 3 on IO.
//!
//! - `lock-hold-and-sleep <base> <runtime> <config> <id>`
//!   Opens the lock, then blocks on stdin (any line ends it).
//!
//! - `daemon-setup-log <path>`
//!   Runs `setup_supervisor_log(path)`, then emits one stdout line,
//!   one stderr line, and one tracing event. Exits 0.

use std::io::{self, Write};
use std::path::PathBuf;

use ark_supervisor::daemon::setup_supervisor_log;
use ark_supervisor::lock::{LockError, acquire_lock};
use ark_types::{SessionId, StateLayout};

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("missing mode");
    match mode.as_str() {
        "lock-acquire-and-exit" => {
            let layout = parse_layout(&mut args);
            let id = parse_id(&mut args);
            match acquire_lock(&layout, &id) {
                Ok(_g) => std::process::exit(0),
                Err(LockError::AlreadyLocked { .. }) => std::process::exit(2),
                Err(LockError::Io(_)) => std::process::exit(3),
            }
        }
        "lock-hold-and-sleep" => {
            let layout = parse_layout(&mut args);
            let id = parse_id(&mut args);
            let _guard = acquire_lock(&layout, &id).expect("acquire");
            let mut line = String::new();
            let _ = io::stdin().read_line(&mut line);
            std::process::exit(0);
        }
        "daemon-setup-log" => {
            let path = PathBuf::from(args.next().expect("missing log path"));
            setup_supervisor_log(&path).expect("setup_supervisor_log");
            println!("stdout-line");
            eprintln!("stderr-line");
            tracing::info!(target: "testhelper", "tracing-line");
            let _ = io::stdout().flush();
            let _ = io::stderr().flush();
            std::process::exit(0);
        }
        other => {
            eprintln!("unknown mode: {other}");
            std::process::exit(64);
        }
    }
}

fn parse_layout(args: &mut impl Iterator<Item = String>) -> StateLayout {
    let base = PathBuf::from(args.next().expect("base"));
    let runtime = PathBuf::from(args.next().expect("runtime"));
    let config = PathBuf::from(args.next().expect("config"));
    StateLayout::new(base, runtime, config)
}

fn parse_id(args: &mut impl Iterator<Item = String>) -> SessionId {
    let id_str = args.next().expect("id");
    SessionId::parse(&id_str).expect("parse id")
}
