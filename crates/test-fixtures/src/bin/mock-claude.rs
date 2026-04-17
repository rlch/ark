//! `mock-claude` — scripted stand-in for the real `claude` CLI used by ark's
//! end-to-end tests (T-126, cavekit-testing R4).
//!
//! The binary reads a JSON script describing a sequence of Claude Code hook
//! events and emits each event in turn, optionally sleeping between them to
//! simulate latency. Tests place this binary's directory first on `PATH` so
//! ark's engine preflight finds it in place of a real Claude session.
//!
//! Two emission modes are supported:
//! - `--output FILE`: JSONL-append each event envelope directly to `FILE`.
//!   Convenient for unit-level tests that just want to see events land.
//! - `--settings SETTINGS_JSON`: resolve hook commands from a Claude
//!   `settings.json` (`hooks.<EventName>[].command`, or the matcher wrapper
//!   shape), and for each event invoke every configured command with the
//!   event JSON piped in on stdin — the same contract the real Claude Code
//!   agent uses.
//!
//! Either flag may be used alone or together (commands run first, then the
//! raw JSONL append). Keeping mock-claude dependency-light (clap + serde_json
//! + std) is deliberate: it must build in the test-fixtures crate without
//! dragging in async runtimes or engine plumbing.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use clap::Parser;
use serde_json::Value;

/// Exit code used for all setup/parsing errors. Kept at 2 to match the
/// conventional "misuse of shell builtins / bad CLI invocation" code so the
/// test harness can distinguish mock-claude failures from scripted exits.
const ERR_EXIT: i32 = 2;

#[derive(Parser, Debug)]
#[command(
    name = "mock-claude",
    about = "Scripted Claude Code stand-in that emits hook events from a JSON script.",
    version
)]
struct Cli {
    /// Path to the scripted events JSON. See module docs for schema.
    #[arg(long)]
    script: PathBuf,

    /// JSONL file to append each emitted event envelope to.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Claude `settings.json` whose `hooks.<Event>[].command` entries should
    /// be invoked with the event JSON piped on stdin.
    #[arg(long)]
    settings: Option<PathBuf>,

    /// Default delay between events, in milliseconds. Per-event `delay_ms`
    /// overrides this value.
    #[arg(long, default_value_t = 0)]
    delay_ms: u64,
}

fn main() {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("mock-claude: {err}");
            std::process::exit(ERR_EXIT);
        }
    }
}

fn run(cli: &Cli) -> Result<i32, String> {
    let raw = fs::read_to_string(&cli.script)
        .map_err(|e| format!("failed to read script {}: {e}", cli.script.display()))?;
    let script: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("failed to parse script {}: {e}", cli.script.display()))?;

    let events = script
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| "script missing required `events` array".to_string())?
        .clone();

    let final_exit = script
        .get("final_exit")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .unwrap_or(0);

    let settings_hooks = match cli.settings.as_deref() {
        Some(path) => Some(load_hook_commands(path)?),
        None => None,
    };

    for (idx, event) in events.iter().enumerate() {
        let event_obj = event
            .as_object()
            .ok_or_else(|| format!("event[{idx}] must be a JSON object"))?;

        let event_name = event_obj
            .get("hook_event_name")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("event[{idx}] missing `hook_event_name`"))?
            .to_string();

        // Per-event delay override; default is --delay-ms.
        let delay = event_obj
            .get("delay_ms")
            .and_then(Value::as_u64)
            .unwrap_or(cli.delay_ms);

        let envelope_json = serde_json::to_string(event)
            .map_err(|e| format!("event[{idx}] re-serialize failed: {e}"))?;

        // 1. Dispatch to any hooks declared in the supplied settings.json.
        if let Some(hooks) = &settings_hooks
            && let Some(cmds) = hooks.get(&event_name)
        {
            for cmd in cmds {
                dispatch_hook(cmd, &envelope_json)
                    .map_err(|e| format!("hook dispatch for {event_name} failed: {e}"))?;
            }
        }

        // 2. Optionally append the raw envelope as JSONL to --output.
        if let Some(out) = &cli.output {
            append_jsonl(out, &envelope_json)
                .map_err(|e| format!("append to {} failed: {e}", out.display()))?;
        }

        if delay > 0 {
            thread::sleep(Duration::from_millis(delay));
        }
    }

    Ok(final_exit)
}

/// Extract the `command` strings under `hooks.<Event>` in a Claude
/// `settings.json`, covering both the flat `{command: "..."}` shape ark emits
/// and the matcher wrapper shape Claude Code uses.
fn load_hook_commands(
    path: &Path,
) -> Result<std::collections::HashMap<String, Vec<String>>, String> {
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("failed to read settings {}: {e}", path.display()))?;
    let v: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("failed to parse settings {}: {e}", path.display()))?;

    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

    let Some(hooks) = v.get("hooks").and_then(Value::as_object) else {
        return Ok(out);
    };

    for (event, entries) in hooks {
        let Some(arr) = entries.as_array() else {
            continue;
        };
        let mut cmds = Vec::new();
        for entry in arr {
            // Shape 1: flat {"command": "..."}
            if let Some(cmd) = entry.get("command").and_then(Value::as_str) {
                cmds.push(cmd.to_string());
                continue;
            }
            // Shape 2: matcher wrapper {"hooks": [{"type": "command", "command": "..."}]}
            if let Some(nested) = entry.get("hooks").and_then(Value::as_array) {
                for n in nested {
                    if let Some(cmd) = n.get("command").and_then(Value::as_str) {
                        cmds.push(cmd.to_string());
                    }
                }
            }
        }
        if !cmds.is_empty() {
            out.insert(event.clone(), cmds);
        }
    }
    Ok(out)
}

/// Spawn `cmd` via the default shell and pipe `payload` to its stdin. Mirrors
/// how Claude Code itself shells out to hook entries.
fn dispatch_hook(cmd: &str, payload: &str) -> Result<(), String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn `{cmd}`: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(payload.as_bytes())
            .map_err(|e| format!("write stdin to `{cmd}`: {e}"))?;
    }
    let _ = child.wait().map_err(|e| format!("wait for `{cmd}`: {e}"))?;
    Ok(())
}

fn append_jsonl(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}
