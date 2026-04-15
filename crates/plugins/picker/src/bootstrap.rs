//! Picker bootstrap (cavekit-plugin-picker R3).
//!
//! Populates the [`PickerCache`] from the host-side filesystem:
//! `$XDG_STATE_HOME/ark/agents/*/status.json` provides the full agent set,
//! and `${XDG_RUNTIME_DIR:-/tmp}/ark-$UID/agents/*.sock` provides liveness.
//! Socket files whose supervisor refuses a 50 ms handshake are unlinked as
//! part of the scan (kakoune `kak -l` GC pattern). The cross-referenced
//! output splits agents into `active` (socket fresh) and `resurrectable`
//! (state present, socket absent, phase not terminal).
//!
//! Everything in this module is pure Rust + std, so host tests can drive it
//! under real tempdirs with no wasm runtime. Re-scanning on the 2 s timer is
//! wired from [`crate::Picker::refresh_cache`].
//!
//! ## JSON parsing rationale
//!
//! The picker ships a hand-rolled JSON field extractor rather than pulling
//! in `serde_json`: cavekit-plugin-picker R1 bans `serde_json`/`humantime`/
//! `chrono` to keep the wasm artefact small (see cavekit-distribution.md
//! R3). Only a handful of fields from `status.json` are needed on the list
//! screen (id, name, orchestrator, engine, phase, cwd, iter, started_at,
//! last_event_at, progress), so a minimal key-driven extractor is cheap and
//! keeps every JSON-handling byte under our control. Unknown / malformed
//! fields are skipped silently — the scan is best-effort per R3.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::time::Duration;

use crate::state::{AgentSummary, PickerCache};

/// Default reachability probe timeout (R3: "50ms connect").
pub const REACHABILITY_TIMEOUT_MS: u64 = 50;

/// Resolve the `(state_dir, runtime_dir)` pair the picker scans.
///
/// Thin wrapper over [`resolve_xdg_paths_with_uid`] that plugs in the
/// canonical UID fallback (`libc::geteuid()` on unix). Callers that want
/// to exercise the no-uid path (host tests, or the no-libc fallback)
/// can call [`resolve_xdg_paths_with_uid`] directly with a closure that
/// returns `None`.
pub fn resolve_xdg_paths(env: impl Fn(&str) -> Option<String>) -> (PathBuf, PathBuf) {
    resolve_xdg_paths_with_uid(env, current_uid_fallback)
}

/// OS-level UID fallback used when the env closure doesn't surface `UID`.
///
/// F-609: zellij plugin harnesses (and many service-launched shells)
/// don't export `UID` — it's a shell-convention variable. Falling back
/// to `libc::geteuid()` gives us the real effective uid so the picker
/// can construct `/tmp/ark-$UID/agents` (or `$XDG_RUNTIME_DIR/ark-$UID/
/// agents`) and find live supervisor sockets. Without this fallback,
/// every live agent was being classified as crashed because the socket
/// scan was being skipped.
#[cfg(unix)]
fn current_uid_fallback() -> Option<String> {
    // SAFETY: geteuid() is infallible and thread-safe on all unix
    // platforms per POSIX.1-2017.
    let uid = unsafe { libc::geteuid() };
    Some(uid.to_string())
}

#[cfg(not(unix))]
fn current_uid_fallback() -> Option<String> {
    None
}

/// Test-friendly variant of [`resolve_xdg_paths`] with an injected UID
/// fallback getter. The production path wires this to
/// [`current_uid_fallback`]; tests can inject `|| None` to verify the
/// "truly no uid available" branch still returns an empty runtime path.
///
/// Precedence mirrors `ark-types::EnvPaths::resolve` (F-604). ark-types is
/// NOT imported here — the picker is WASM-only at runtime and pulling the
/// crate in would break that constraint; the host-side env injector
/// (see call sites in `Picker::refresh_cache`) is the source of truth,
/// this function just consumes the injected closure.
///
/// State side:
///   1. `$ARK_STATE_DIR` — verbatim (no `ark/` suffix; caller chose an
///      isolated path).
///   2. `$XDG_STATE_HOME/ark` — XDG fallback.
///   3. `$HOME/.local/state/ark` — platform fallback.
///   4. Empty `PathBuf` — caller skips the state scan.
///
/// Runtime side (option D2, mirrors `ark_types::env_paths::resolve_runtime`):
///   1. `$ARK_RUNTIME_DIR/agents` — verbatim (no `ark-$UID` segment);
///      matches `EnvPaths::resolve_runtime` semantics.
///   2. `$XDG_RUNTIME_DIR/ark-$UID/agents` — Linux systemd idiom.
///   3. `$TMPDIR/ark/agents` — macOS idiom. `$TMPDIR` is already a
///      per-user sandboxed path, so no `ark-$UID` disambiguator is
///      needed and the path stays pretty. UID missing is fine here.
///   4. `/tmp/ark-$UID/agents` — bare-Linux last resort, UID sourced
///      from env closure or `uid_fallback` (F-609).
///   5. When none of 1-3 apply and the uid is unrecoverable (env
///      closure + `uid_fallback` both return `None`), we cannot
///      construct a safe per-user path on `/tmp`
///      (`/tmp/ark/agents` would collide across users on a multi-tenant
///      host). The runtime side then returns `PathBuf::new()` and the
///      caller skips the socket scan — pipe-only liveness still works.
pub fn resolve_xdg_paths_with_uid(
    env: impl Fn(&str) -> Option<String>,
    uid_fallback: impl FnOnce() -> Option<String>,
) -> (PathBuf, PathBuf) {
    // --- state_dir ---
    let state_dir = if let Some(ark) = env("ARK_STATE_DIR").filter(|s| !s.is_empty()) {
        PathBuf::from(ark)
    } else if let Some(xdg) = env("XDG_STATE_HOME").filter(|s| !s.is_empty()) {
        PathBuf::from(xdg).join("ark")
    } else if let Some(home) = env("HOME").filter(|s| !s.is_empty()) {
        PathBuf::from(home).join(".local").join("state").join("ark")
    } else {
        PathBuf::new()
    };

    // --- runtime_dir ---
    // 1. $ARK_RUNTIME_DIR — verbatim. Caller already isolated the path.
    if let Some(ark_rt) = env("ARK_RUNTIME_DIR").filter(|s| !s.is_empty()) {
        return (state_dir, PathBuf::from(ark_rt).join("agents"));
    }

    // For $XDG_RUNTIME_DIR and `/tmp` branches we need a UID to
    // disambiguate between users on a shared host. Prefer `env("UID")`
    // (shell convention) but fall back to the uid_fallback closure —
    // F-609: zellij plugin harnesses routinely don't export UID, and
    // without a fallback every live agent was being classified as crashed.
    let uid = env("UID")
        .filter(|s| !s.is_empty())
        .or_else(uid_fallback)
        .filter(|s| !s.is_empty());

    // 2. $XDG_RUNTIME_DIR/ark-$UID/agents.
    if let Some(xdg) = env("XDG_RUNTIME_DIR").filter(|s| !s.is_empty()) {
        let Some(uid) = uid else {
            return (state_dir, PathBuf::new());
        };
        return (
            state_dir,
            PathBuf::from(xdg).join(format!("ark-{uid}")).join("agents"),
        );
    }

    // 3. $TMPDIR/ark/agents — macOS idiom. $TMPDIR is already per-user,
    // no uid needed; works even when UID isn't recoverable.
    if let Some(tmpdir) = env("TMPDIR").filter(|s| !s.is_empty()) {
        return (state_dir, PathBuf::from(tmpdir).join("ark").join("agents"));
    }

    // 4. /tmp/ark-$UID/agents — bare-Linux last resort.
    let Some(uid) = uid else {
        return (state_dir, PathBuf::new());
    };
    let runtime_dir = PathBuf::from("/tmp")
        .join(format!("ark-{uid}"))
        .join("agents");

    (state_dir, runtime_dir)
}

/// Classification of an agent after cross-referencing state + socket.
///
/// R3: socket fresh → running (active); socket absent + phase terminal
/// (`done`/`failed`/`killed`/`timeout`) → done (terminal, no resurrect);
/// socket absent + phase non-terminal (`running`, `starting`, `idle`, …) →
/// crashed (resurrectable via T-106 `r`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Live supervisor — socket answered within the reachability window.
    Active,
    /// Terminal phase with no live supervisor — keep listed for context
    /// but don't offer resurrect.
    Done,
    /// Non-terminal phase with a dead supervisor — eligible for R7 `r`.
    Crashed,
    /// Nothing to show (socket absent, no state) — caller should skip.
    Skip,
}

/// Phases that the cross-reference logic considers "terminal" — i.e. the
/// agent is meant to be gone. Matches `ark-types::Phase`'s terminal set
/// (`Done`, `Failed`, `Crashed`, `Killed`, `Timeout`).
///
/// `Crashed` is included so that a state file already flagged as crashed
/// stays in the Done bucket instead of offering resurrect twice.
fn is_terminal_phase(phase: &str) -> bool {
    matches!(phase, "done" | "failed" | "crashed" | "killed" | "timeout")
}

/// Decide whether to treat the agent as active, terminal, crashed, or skip.
pub fn classify(state_summary: Option<&AgentSummary>, socket_fresh: bool) -> Classification {
    match (state_summary, socket_fresh) {
        // Socket answered — agent is alive regardless of what the on-disk
        // phase claims (pipe updates catch up shortly).
        (_, true) => Classification::Active,
        // No socket + terminal state → Done bucket.
        (Some(s), false) if is_terminal_phase(&s.phase) => Classification::Done,
        // No socket + non-terminal state → supervisor died mid-run.
        (Some(_), false) => Classification::Crashed,
        // No state + no socket → shouldn't happen in steady state; skip.
        (None, false) => Classification::Skip,
    }
}

/// Extract the raw string value for a top-level `"key"` from a JSON object.
///
/// Intentionally naive: locates the first `"<key>"` followed by `:`, skips
/// whitespace, then returns the content of the next string (handling only
/// simple backslash escapes: `\"`, `\\`, `\n`, `\t`). Returns `None` if the
/// key is missing, the value isn't a string, or the JSON is malformed.
///
/// Good enough for the 8 top-level fields in `status.json` that the picker
/// needs — a real JSON parser would cost ~100kB of wasm. Nested objects
/// must be located with a dedicated scan (see `find_object`).
pub(crate) fn find_string_field(json: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let start = find_key_colon(json, &pat)?;
    let rest = &json[start..];
    parse_json_string(rest)
}

/// Extract the raw integer / float value for a top-level `"key"`, best-
/// effort parsed as `u64`. Numbers with fractional parts are truncated by
/// the `parse::<u64>()` branch failing — callers treat that as "missing".
pub(crate) fn find_u64_field(json: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{key}\"");
    let start = find_key_colon(json, &pat)?;
    let slice = json[start..].trim_start();
    // Collect digits.
    let bytes = slice.as_bytes();
    let mut end = 0;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == 0 {
        return None;
    }
    slice[..end].parse::<u64>().ok()
}

/// Extract a timestamp field that may be serialised either as a numeric
/// epoch-seconds (test-fixture / legacy shape) or as an ISO-8601 string
/// (chrono `DateTime<Utc>` default, which is what supervisors write).
/// Returns epoch-seconds in both cases, or `None` if parse fails.
///
/// F-612: the supervisor emits `"last_event_at":"2026-04-15T04:30:00Z"`
/// to `status.json` via chrono's default Serialize impl. Before this
/// fix the picker only ran `find_u64_field`, which rejected the string
/// and left `last_event_at` as `None` for every real agent — so the
/// list screen's age column was always blank and the age-sort was
/// effectively disabled.
///
/// Returning seconds (not milliseconds) keeps the numeric path
/// behaviourally identical to `find_u64_field` so all downstream code
/// (e.g. `render_list.rs::format_row` which calls
/// `ts.saturating_mul(1000)`) keeps working unchanged.
pub(crate) fn find_timestamp_field(json: &str, key: &str) -> Option<u64> {
    if let Some(secs) = find_u64_field(json, key) {
        return Some(secs);
    }
    let s = find_string_field(json, key)?;
    iso8601_to_epoch_secs(&s)
}

/// Parse a minimal subset of ISO-8601 into epoch seconds.
///
/// Accepts `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH[:]MM]`. Fractional seconds are
/// dropped (second-precision is enough for the picker's age column).
/// Timezone offsets are honoured; missing offset treated as UTC.
///
/// Hand-rolled rather than pulling chrono/humantime into the picker's
/// wasm budget per cavekit-plugin-picker R1 — only a handful of fields
/// in `status.json` need this and the logic is ~60 lines of pure arith.
pub(crate) fn iso8601_to_epoch_secs(s: &str) -> Option<u64> {
    let (date_part, time_part) = s.split_once('T')?;

    // --- date: YYYY-MM-DD ---
    let mut date_iter = date_part.split('-');
    let year: i64 = date_iter.next()?.parse().ok()?;
    let month: u32 = date_iter.next()?.parse().ok()?;
    let day: u32 = date_iter.next()?.parse().ok()?;
    if date_iter.next().is_some() {
        return None;
    }
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // --- split time from optional offset / Z ---
    // Strategy: find the first char after position 0 that is `Z`, `+`, or
    // `-` (the ISO offset markers). Everything before it is the HH:MM:SS
    // [.fff] body; everything after is the offset.
    let (time_body, offset_str) = split_iso_offset(time_part);

    // --- time: HH:MM:SS[.fff] — drop fractional seconds ---
    let time_no_frac = time_body.split('.').next()?;
    let mut time_iter = time_no_frac.split(':');
    let hour: u32 = time_iter.next()?.parse().ok()?;
    let minute: u32 = time_iter.next()?.parse().ok()?;
    let second: u32 = time_iter.next()?.parse().ok()?;
    if time_iter.next().is_some() {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    // --- offset: Z | ±HH[:]MM ---
    let offset_secs: i64 = match offset_str {
        "" | "Z" | "z" => 0,
        rest => parse_iso_offset_secs(rest)?,
    };

    // --- build epoch seconds from proleptic-Gregorian date ---
    let days = days_from_civil(year, month, day)?;
    let secs = days as i64 * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    let secs = secs.checked_sub(offset_secs)?;
    if secs < 0 {
        return None;
    }
    Some(secs as u64)
}

/// Peel off the trailing ISO-8601 offset (Z, +HH:MM, -HHMM, …) from a
/// time string. Returns `(body_without_offset, offset_str)` where
/// `offset_str` is empty if no offset was present.
fn split_iso_offset(time_part: &str) -> (&str, &str) {
    let bytes = time_part.as_bytes();
    for (i, &b) in bytes.iter().enumerate().skip(1) {
        if b == b'Z' || b == b'z' || b == b'+' || b == b'-' {
            return (&time_part[..i], &time_part[i..]);
        }
    }
    (time_part, "")
}

/// Parse `±HH:MM` or `±HHMM` → offset in seconds. Caller peels the `Z`
/// case separately.
fn parse_iso_offset_secs(s: &str) -> Option<i64> {
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1i64, &s[1..]),
        b'-' => (-1i64, &s[1..]),
        _ => return None,
    };
    let rest = rest.replace(':', "");
    if rest.len() != 4 {
        return None;
    }
    let hh: i64 = rest[..2].parse().ok()?;
    let mm: i64 = rest[2..].parse().ok()?;
    if hh > 23 || mm > 59 {
        return None;
    }
    Some(sign * (hh * 3600 + mm * 60))
}

/// Days since 1970-01-01 for `(year, month, day)` in the proleptic
/// Gregorian calendar. Implementation from Howard Hinnant's well-known
/// `days_from_civil` algorithm (public domain, <http://howardhinnant.github.io/date_algorithms.html>).
/// Returns `None` for years < 1970 (our on-disk timestamps never go that
/// far back — they're bounded by supervisor start).
fn days_from_civil(y: i64, m: u32, d: u32) -> Option<i64> {
    if y < 1970 {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as i64; // [0, 399]
    let m_adj = if m > 2 { m as i64 - 3 } else { m as i64 + 9 };
    let doy = (153 * m_adj + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    Some(era * 146097 + doe - 719468)
}

/// Extract a `(u32, u32)` two-element array for a top-level `"key"`, e.g.
/// `"progress":[3,10]`. Returns `None` if the field is `null`, missing, or
/// not a 2-element numeric array.
fn find_progress_field(json: &str, key: &str) -> Option<(u32, u32)> {
    let pat = format!("\"{key}\"");
    let start = find_key_colon(json, &pat)?;
    let slice = json[start..].trim_start();
    let mut chars = slice.chars();
    if chars.next()? != '[' {
        return None;
    }
    // Locate matching `]`.
    let rest = &slice[1..];
    let end_rel = rest.find(']')?;
    let inner = &rest[..end_rel];
    let mut parts = inner.split(',').map(|s| s.trim());
    let a = parts.next()?.parse::<u32>().ok()?;
    let b = parts.next()?.parse::<u32>().ok()?;
    Some((a, b))
}

/// Extract the JSON object body (content between `{` and matching `}`) for
/// a top-level `"key":{ ... }`. Respects brace depth so nested objects work
/// correctly. Returns `None` if the field is missing or not an object.
pub(crate) fn find_object_field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\"");
    let start = find_key_colon(json, &pat)?;
    let slice = &json[start..];
    let trimmed = slice.trim_start();
    let bytes = trimmed.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if b == b'\\' && in_string {
            escaped = true;
            continue;
        }
        if b == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    // Exclusive of outer braces.
                    return std::str::from_utf8(&bytes[1..i]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

/// Advance past `"<key>"` and the following `:`, returning the byte offset
/// into `json` where the value begins. Returns `None` if the key isn't
/// present or no `:` follows.
fn find_key_colon(json: &str, pat: &str) -> Option<usize> {
    let mut search_from = 0;
    loop {
        let rel = json[search_from..].find(pat)?;
        let key_start = search_from + rel;
        // Sanity: require the char before `pat` to NOT be another quote
        // letter — guards against matching `"foo_bar"` when looking for
        // `"bar"`. `{`, `,`, or whitespace is what we expect.
        let preceding_ok = key_start == 0
            || matches!(
                json.as_bytes().get(key_start - 1),
                Some(b'{' | b',' | b' ' | b'\n' | b'\r' | b'\t')
            );
        if !preceding_ok {
            search_from = key_start + pat.len();
            continue;
        }
        let after_key = key_start + pat.len();
        let remainder = &json[after_key..];
        let trimmed = remainder.trim_start();
        if !trimmed.starts_with(':') {
            // Not a key-value pair shape; keep searching.
            search_from = after_key;
            continue;
        }
        // Return offset to char just past the colon, skipping leading
        // whitespace of the value itself.
        let colon_offset = remainder.len() - trimmed.len() + 1;
        let value_start = after_key + colon_offset;
        return Some(value_start);
    }
}

/// Parse a JSON string literal starting at the leading `"` inside `slice`
/// (after any leading whitespace). Supports `\"`, `\\`, `\n`, `\t`, `\r`
/// escapes; anything else becomes its literal char (including `\u`, which
/// we don't try to decode — acceptable for the ASCII fields the picker
/// cares about).
fn parse_json_string(slice: &str) -> Option<String> {
    let trimmed = slice.trim_start();
    let bytes = trimmed.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut i = 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            i += 1;
            if i >= bytes.len() {
                return None;
            }
            match bytes[i] {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'n' => out.push('\n'),
                b't' => out.push('\t'),
                b'r' => out.push('\r'),
                b'/' => out.push('/'),
                other => out.push(other as char),
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            return Some(out);
        }
        out.push(b as char);
        i += 1;
    }
    None
}

/// Parse the minimal shape of `status.json` the picker needs.
///
/// Pulls:
/// - `spec.id` (as string via `AgentId::Display` — the supervisor serialises
///   the nested `AgentId` struct with at least an `id` or `.to_string()`
///   display; we try several common shapes).
/// - `spec.name`, `spec.orchestrator`, `spec.engine`, `spec.cwd`.
/// - top-level `phase`, `progress`, `last_event_at`.
///
/// Missing optional fields (iter, started_at) are `None`. If neither `id`
/// nor `name` can be found the parse fails — we need at least one to key
/// the cache.
pub fn parse_agent_status_minimal(s: &str) -> Option<AgentSummary> {
    // Extract the `"spec":{...}` subobject first.
    let spec = find_object_field(s, "spec").unwrap_or("");
    // `spec.id` — AgentId serializes to a struct; try the flattened
    // `"id":"..."` form (which matches ark-types' AgentId Serialize impl
    // producing a JSON string) as well as a nested object that carries an
    // inner `"id"` field (older shape).
    let id = find_string_field(spec, "id")
        .or_else(|| find_object_field(spec, "id").and_then(|o| find_string_field(o, "id")))
        .unwrap_or_default();

    let name = find_string_field(spec, "name").unwrap_or_default();
    // F-601: real zellij session identifier; `spec.session` carries the
    // suffixed `ark-{orch}-{name}-{ulid8}` form after F-600. Older
    // status.json files written before F-600 may still have the bare
    // `ark-{orch}-{name}` form — we surface whatever is on disk and let
    // the Enter handler pass it through verbatim.
    let session = find_string_field(spec, "session").unwrap_or_default();
    let orchestrator = find_string_field(spec, "orchestrator").unwrap_or_default();
    let engine = find_string_field(spec, "engine").unwrap_or_default();
    let cwd = find_string_field(spec, "cwd").unwrap_or_default();

    let phase = find_string_field(s, "phase").unwrap_or_default();
    let progress = find_progress_field(s, "progress");

    // Timestamps: supervisors write `last_event_at` / `started_at` as an
    // ISO-8601 string (chrono default serialises `DateTime<Utc>` to
    // `"2026-04-15T04:30:00Z"`). F-612: previously this site only tried
    // `find_u64_field`, so the ISO-8601 form produced by real supervisors
    // returned `None` and the list screen's age column stayed blank /
    // sorted agents incorrectly. We now accept both: numeric epoch-ms
    // first (older shape / test fixtures), then ISO-8601 string.
    let started_at = find_timestamp_field(s, "started_at");
    let last_event_at = find_timestamp_field(s, "last_event_at");

    if id.is_empty() && name.is_empty() {
        return None;
    }

    Some(AgentSummary {
        id,
        name,
        session,
        orchestrator,
        engine,
        phase,
        cwd,
        iter: None,
        started_at,
        last_event_at,
        progress,
    })
}

/// Enumerate `state_dir/agents/<id>/status.json` and return the parseable
/// entries. Silently skips unreadable directories and malformed JSON —
/// best-effort per R3. Missing `state_dir` returns an empty vec.
pub fn scan_state_dir(state_dir: &Path) -> Vec<AgentSummary> {
    let agents_root = state_dir.join("agents");
    let Ok(read_dir) = fs::read_dir(&agents_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let status_json = path.join("status.json");
        let Ok(contents) = fs::read_to_string(&status_json) else {
            continue;
        };
        let Some(mut summary) = parse_agent_status_minimal(&contents) else {
            continue;
        };
        // Backfill id from the subdir name if the parser couldn't pull it
        // out of the JSON — `state_dir/agents/<id>/status.json` is the
        // canonical source of truth for the id regardless of spec shape.
        if summary.id.is_empty()
            && let Some(dir_name) = path.file_name().and_then(|s| s.to_str())
        {
            summary.id = dir_name.to_string();
        }
        out.push(summary);
    }
    out
}

/// Enumerate `*.sock` files directly under `runtime_dir` and return their
/// agent-id stems (filename without the `.sock` extension).
///
/// Directory misses and non-socket entries are silently skipped; the caller
/// is expected to treat an empty result as "no live agents". Does NOT
/// recurse.
pub fn scan_socket_dir(runtime_dir: &Path) -> Vec<String> {
    let Ok(read_dir) = fs::read_dir(runtime_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".sock") else {
            continue;
        };
        if stem.is_empty() {
            continue;
        }
        out.push(stem.to_string());
    }
    out
}

/// Attempt a `connect_timeout` against `sock_path` — returns `true` iff a
/// supervisor is bound and accepts the handshake within `timeout_ms`.
///
/// On non-unix targets (host tests only — the actual plugin is wasm32)
/// returns `false`, matching R3's "socket present = supervisor still bound"
/// contract: we have no way to probe without `UnixStream`, so we err on the
/// side of "not reachable" and let stale-GC clean up.
pub fn check_reachable(sock_path: &Path, timeout_ms: u64) -> bool {
    #[cfg(unix)]
    {
        // `UnixStream::connect` does not take a timeout directly (std's
        // `connect_timeout` is Tcp-only). A 50 ms budget on a local unix
        // socket is effectively instant: an un-bound / stale path returns
        // ECONNREFUSED immediately, and a bound listener accepts without
        // blocking. So we rely on connect's natural fast-fail and only use
        // `timeout_ms` to bound any subsequent read we choose to perform.
        match UnixStream::connect(sock_path) {
            Ok(stream) => {
                // Clear the timeouts immediately; we only needed to know
                // that `connect()` returned ok. The blocking `connect()`
                // above does not take a timeout in std, so we approximate
                // the 50 ms budget by setting a non-blocking read timeout
                // then dropping the stream.
                let _ = stream.set_read_timeout(Some(Duration::from_millis(timeout_ms)));
                drop(stream);
                true
            }
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (sock_path, timeout_ms);
        false
    }
}

/// Remove `.sock` files under `runtime_dir` whose supervisor has died.
///
/// For every `*.sock` file, `connect_timeout` is attempted; if it fails the
/// file is `unlink`ed so the next scan doesn't keep classifying a dead
/// agent as active. Returns the number of files removed so the caller can
/// decide whether to redraw.
pub fn gc_stale_sockets(runtime_dir: &Path) -> usize {
    let Ok(read_dir) = fs::read_dir(runtime_dir) else {
        return 0;
    };
    let mut removed = 0usize;
    for entry in read_dir.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.ends_with(".sock") {
            continue;
        }
        if !check_reachable(&path, REACHABILITY_TIMEOUT_MS) {
            let _ = fs::remove_file(&path);
            removed += 1;
        }
    }
    removed
}

/// Orchestrate a full bootstrap pass:
///
/// 1. Scan `state_dir` for `status.json` summaries.
/// 2. Scan `runtime_dir` for `.sock` files.
/// 3. Probe each socket with [`check_reachable`]; un-reachable sockets are
///    unlinked (stale GC — R3).
/// 4. Cross-reference via [`classify`] to split into active vs resurrectable.
///
/// Returns a fully-populated [`PickerCache`]. Idempotent; safe to call on
/// every 2 s timer tick (state-dir read is O(agents), socket probes are
/// bounded by REACHABILITY_TIMEOUT_MS × live agents).
pub fn bootstrap(state_dir: &Path, runtime_dir: &Path) -> PickerCache {
    let state_entries = scan_state_dir(state_dir);
    let mut state_by_id: BTreeMap<String, AgentSummary> = state_entries
        .into_iter()
        .filter(|s| !s.id.is_empty())
        .map(|s| (s.id.clone(), s))
        .collect();

    // Scan sockets and probe each. Unreachable → unlink and mark absent.
    let socket_ids = scan_socket_dir(runtime_dir);
    let mut active_ids = BTreeMap::<String, ()>::new();
    for id in socket_ids {
        let sock_path = runtime_dir.join(format!("{id}.sock"));
        if check_reachable(&sock_path, REACHABILITY_TIMEOUT_MS) {
            active_ids.insert(id, ());
        } else {
            // Stale socket — remove so next pass doesn't re-probe it.
            let _ = fs::remove_file(&sock_path);
        }
    }

    let mut cache = PickerCache::default();
    // First pass: classify every state entry.
    let ids: Vec<String> = state_by_id.keys().cloned().collect();
    for id in ids {
        let summary = state_by_id.remove(&id).expect("key from snapshot");
        let fresh = active_ids.contains_key(&id);
        match classify(Some(&summary), fresh) {
            Classification::Active => {
                cache.active.insert(summary.id.clone(), summary);
            }
            Classification::Done => {
                // Done/failed/killed/timeout agents stay visible via the
                // resurrectable bucket with their terminal phase intact;
                // the list screen renders them with a `✓`/`✗` icon and
                // does NOT offer the `[R]` resurrect affordance (phase
                // decides). Keeping them in `resurrectable` rather than
                // inventing a third bucket matches the kit's "separate
                // cache for crashed agents" wording while keeping the
                // terminal-phase agents out of the active list.
                cache.resurrectable.insert(summary.id.clone(), summary);
            }
            Classification::Crashed => {
                cache.resurrectable.insert(summary.id.clone(), summary);
            }
            Classification::Skip => {}
        }
    }
    // Second pass: any socket without a state entry gets a placeholder
    // active summary — matches the pipe-incremental path's best-effort
    // semantics (caller usually hears from the supervisor within a tick).
    for (id, _) in active_ids {
        if !cache.active.contains_key(&id) && !cache.resurrectable.contains_key(&id) {
            cache.active.insert(
                id.clone(),
                AgentSummary {
                    id,
                    phase: "running".to_string(),
                    ..AgentSummary::default()
                },
            );
        }
    }
    cache
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    // ------------------------------------------------------------------ tempdir

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let p = std::env::temp_dir().join(format!(
                "ark-picker-bootstrap-{}-{}-{}",
                tag,
                std::process::id(),
                n
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_status_json(state_root: &Path, id: &str, phase: &str, name: &str) {
        let dir = state_root.join("agents").join(id);
        fs::create_dir_all(&dir).unwrap();
        // Shape matches what ark-types writes: nested spec{} + top-level
        // phase / progress / last_event_at. We keep it minimal so the
        // hand-rolled parser is actually exercised on the exact fields it
        // cares about.
        let json = format!(
            r#"{{
                "spec":{{"id":"{id}","name":"{name}","orchestrator":"cavekit","engine":"claude-code","cwd":"/tmp/{id}"}},
                "phase":"{phase}",
                "progress":[3,10],
                "last_event_at":"2026-04-15T00:00:00Z",
                "started_at":1700000000
            }}"#
        );
        fs::write(dir.join("status.json"), json).unwrap();
    }

    // --- parse_agent_status_minimal -------------------------------------

    #[test]
    fn parse_happy_path() {
        let json = r#"{"spec":{"id":"ark-cavekit-auth","name":"auth","session":"ark-cavekit-auth-ABCDEFGH","orchestrator":"cavekit","engine":"claude-code","cwd":"/tmp/w"},"phase":"running","progress":[3,10],"started_at":1700000000}"#;
        let s = parse_agent_status_minimal(json).expect("parse");
        assert_eq!(s.id, "ark-cavekit-auth");
        assert_eq!(s.name, "auth");
        // F-601: session carries the real suffixed zellij identifier.
        assert_eq!(s.session, "ark-cavekit-auth-ABCDEFGH");
        assert_eq!(s.orchestrator, "cavekit");
        assert_eq!(s.engine, "claude-code");
        assert_eq!(s.phase, "running");
        assert_eq!(s.cwd, "/tmp/w");
        assert_eq!(s.progress, Some((3, 10)));
        assert_eq!(s.started_at, Some(1_700_000_000));
    }

    #[test]
    fn parse_session_missing_defaults_empty() {
        // F-601: legacy status.json written pre-F-600 has no `session`
        // in `spec{}` — parser must tolerate this and the Enter handler
        // then falls back to `summary.name`.
        let json = r#"{"spec":{"id":"x","name":"y","orchestrator":"cavekit","engine":"claude-code","cwd":"/tmp"},"phase":"running"}"#;
        let s = parse_agent_status_minimal(json).expect("parse");
        assert!(s.session.is_empty(), "missing session parses as empty");
    }

    #[test]
    fn parse_missing_spec_returns_none_when_empty() {
        let json = r#"{"phase":"running"}"#;
        assert!(parse_agent_status_minimal(json).is_none());
    }

    #[test]
    fn parse_tolerates_missing_optional_fields() {
        let json = r#"{"spec":{"id":"x","name":"y"},"phase":"idle"}"#;
        let s = parse_agent_status_minimal(json).expect("parse");
        assert_eq!(s.id, "x");
        assert_eq!(s.phase, "idle");
        assert_eq!(s.progress, None);
        assert_eq!(s.started_at, None);
    }

    #[test]
    fn parse_ignores_similar_key_suffixes() {
        // `not_name` must not satisfy a lookup for `name`.
        let json = r#"{"spec":{"id":"x","not_name":"ignore","name":"real"},"phase":"p"}"#;
        let s = parse_agent_status_minimal(json).unwrap();
        assert_eq!(s.name, "real");
    }

    // --- F-612: ISO-8601 timestamp parsing ------------------------------

    #[test]
    fn iso8601_utc_round_number() {
        // 2026-04-15T00:00:00Z = 1_776_211_200 seconds since epoch
        // (verified via `date -u -j -f %FT%TZ 2026-04-15T00:00:00Z +%s`).
        assert_eq!(
            iso8601_to_epoch_secs("2026-04-15T00:00:00Z"),
            Some(1_776_211_200)
        );
    }

    #[test]
    fn iso8601_with_fractional_and_offset() {
        // Fractional seconds are dropped; +02:00 offset subtracts 2h.
        // 2026-04-15T02:00:00+02:00 is exactly 2026-04-15T00:00:00Z.
        assert_eq!(
            iso8601_to_epoch_secs("2026-04-15T02:00:00.123+02:00"),
            Some(1_776_211_200)
        );
    }

    #[test]
    fn iso8601_rejects_malformed() {
        assert!(iso8601_to_epoch_secs("not-a-date").is_none());
        assert!(iso8601_to_epoch_secs("2026-04-15").is_none()); // no T
        assert!(iso8601_to_epoch_secs("1969-01-01T00:00:00Z").is_none()); // pre-1970
        assert!(iso8601_to_epoch_secs("2026-13-15T00:00:00Z").is_none()); // bad month
    }

    #[test]
    fn parse_agent_status_accepts_iso8601_timestamps() {
        // Real status.json shape: `last_event_at` is a chrono
        // ISO-8601 string, not a number. F-612: before this fix
        // `last_event_at` was always None for real agents.
        let json = r#"{"spec":{"id":"x","name":"y"},"phase":"running","started_at":"2026-04-15T00:00:00Z","last_event_at":"2026-04-15T00:00:30Z"}"#;
        let s = parse_agent_status_minimal(json).expect("parse");
        assert_eq!(s.started_at, Some(1_776_211_200));
        assert_eq!(s.last_event_at, Some(1_776_211_230));
    }

    #[test]
    fn parse_agent_status_still_accepts_numeric_timestamps() {
        // Backcompat: the numeric-epoch-seconds shape (used by existing
        // tests and any tooling that wrote status.json before chrono's
        // ISO-8601 default landed) must continue to parse.
        let json = r#"{"spec":{"id":"x","name":"y"},"phase":"running","started_at":1700000000,"last_event_at":1700000500}"#;
        let s = parse_agent_status_minimal(json).expect("parse");
        assert_eq!(s.started_at, Some(1_700_000_000));
        assert_eq!(s.last_event_at, Some(1_700_000_500));
    }

    // --- scan_state_dir --------------------------------------------------

    #[test]
    fn scan_state_dir_returns_all_valid_entries() {
        let tmp = TempDir::new("state-valid");
        write_status_json(tmp.path(), "a1", "running", "one");
        write_status_json(tmp.path(), "a2", "done", "two");
        write_status_json(tmp.path(), "a3", "idle", "three");

        let mut out = scan_state_dir(tmp.path());
        out.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].id, "a1");
        assert_eq!(out[1].phase, "done");
        assert_eq!(out[2].name, "three");
    }

    #[test]
    fn scan_state_dir_skips_malformed_json() {
        let tmp = TempDir::new("state-bad");
        write_status_json(tmp.path(), "good", "running", "g");
        let bad = tmp.path().join("agents").join("bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("status.json"), "{not json").unwrap();

        let out = scan_state_dir(tmp.path());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "good");
    }

    #[test]
    fn scan_state_dir_missing_returns_empty() {
        let tmp = TempDir::new("state-missing");
        let out = scan_state_dir(tmp.path());
        assert!(out.is_empty());
    }

    // --- scan_socket_dir -------------------------------------------------

    #[test]
    fn scan_socket_dir_returns_sock_stems() {
        let tmp = TempDir::new("sock");
        fs::write(tmp.path().join("a.sock"), b"").unwrap();
        fs::write(tmp.path().join("b.sock"), b"").unwrap();
        fs::write(tmp.path().join("c.sock"), b"").unwrap();
        fs::write(tmp.path().join("not-a-socket"), b"").unwrap();

        let mut ids = scan_socket_dir(tmp.path());
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "b".into(), "c".into()]);
    }

    #[test]
    fn scan_socket_dir_missing_returns_empty() {
        let tmp = TempDir::new("sock-missing");
        let out = scan_socket_dir(&tmp.path().join("does-not-exist"));
        assert!(out.is_empty());
    }

    // --- check_reachable / gc_stale_sockets ------------------------------

    #[test]
    fn check_reachable_against_fake_socket_is_false() {
        let tmp = TempDir::new("reachable");
        // A regular file named `.sock` is NOT a bound socket → connect
        // fails → check_reachable returns false.
        let p = tmp.path().join("stub.sock");
        fs::write(&p, b"").unwrap();
        assert!(!check_reachable(&p, 50));
    }

    #[test]
    fn gc_stale_sockets_removes_unreachable() {
        let tmp = TempDir::new("gc");
        fs::write(tmp.path().join("a.sock"), b"").unwrap();
        fs::write(tmp.path().join("b.sock"), b"").unwrap();
        let removed = gc_stale_sockets(tmp.path());
        assert_eq!(removed, 2);
        // Both unlinked.
        assert!(!tmp.path().join("a.sock").exists());
        assert!(!tmp.path().join("b.sock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn gc_stale_sockets_keeps_live_socket() {
        use std::os::unix::net::UnixListener;
        let tmp = TempDir::new("gc-live");
        let live_path = tmp.path().join("live.sock");
        let _listener = UnixListener::bind(&live_path).unwrap();
        fs::write(tmp.path().join("dead.sock"), b"").unwrap();

        let removed = gc_stale_sockets(tmp.path());
        // live.sock answered connect → survives; dead.sock → unlinked.
        assert_eq!(removed, 1);
        assert!(live_path.exists());
        assert!(!tmp.path().join("dead.sock").exists());
    }

    // --- classify --------------------------------------------------------

    fn mk_summary(phase: &str) -> AgentSummary {
        AgentSummary {
            id: "x".into(),
            phase: phase.into(),
            ..AgentSummary::default()
        }
    }

    #[test]
    fn classify_socket_fresh_overrides_state() {
        // Socket wins even if state claims Done.
        let s = mk_summary("done");
        assert_eq!(classify(Some(&s), true), Classification::Active);
    }

    #[test]
    fn classify_socket_absent_terminal_is_done() {
        let s = mk_summary("done");
        assert_eq!(classify(Some(&s), false), Classification::Done);
    }

    #[test]
    fn classify_socket_absent_running_is_crashed() {
        let s = mk_summary("running");
        assert_eq!(classify(Some(&s), false), Classification::Crashed);
    }

    #[test]
    fn classify_socket_absent_failed_is_done() {
        // `failed` is terminal — do NOT offer resurrect.
        let s = mk_summary("failed");
        assert_eq!(classify(Some(&s), false), Classification::Done);
    }

    #[test]
    fn classify_no_state_no_socket_is_skip() {
        assert_eq!(classify(None, false), Classification::Skip);
    }

    // --- bootstrap integration ------------------------------------------

    #[test]
    fn bootstrap_splits_active_and_resurrectable() {
        use std::os::unix::net::UnixListener;

        let state_tmp = TempDir::new("boot-state");
        let rt_tmp = TempDir::new("boot-rt");

        // state: 3 agents
        write_status_json(state_tmp.path(), "alive", "running", "alive-agent");
        write_status_json(state_tmp.path(), "crashed", "running", "crashed-agent");
        write_status_json(state_tmp.path(), "finished", "done", "done-agent");

        // runtime: only `alive` has a live listener, `stale` has a dead sock
        let alive_sock = rt_tmp.path().join("alive.sock");
        let _listener = UnixListener::bind(&alive_sock).unwrap();
        fs::write(rt_tmp.path().join("stale.sock"), b"").unwrap();

        let cache = bootstrap(state_tmp.path(), rt_tmp.path());

        // alive: live socket → active
        assert!(
            cache.active.contains_key("alive"),
            "active expected: {:?}",
            cache
        );
        // crashed: state present, no socket, phase=running → resurrectable
        assert!(cache.resurrectable.contains_key("crashed"));
        // finished: state present, no socket, phase=done → resurrectable bucket too
        assert!(cache.resurrectable.contains_key("finished"));
        assert_eq!(cache.resurrectable["finished"].phase, "done");

        // stale.sock should be unlinked.
        assert!(!rt_tmp.path().join("stale.sock").exists());
    }

    // --- resolve_xdg_paths ----------------------------------------------

    #[test]
    fn resolve_xdg_paths_prefers_xdg_env() {
        let env = |k: &str| match k {
            "XDG_STATE_HOME" => Some("/state".to_string()),
            "XDG_RUNTIME_DIR" => Some("/run".into()),
            "UID" => Some("1000".into()),
            _ => None,
        };
        let (state, rt) = resolve_xdg_paths(env);
        assert_eq!(state, PathBuf::from("/state/ark"));
        assert_eq!(rt, PathBuf::from("/run/ark-1000/agents"));
    }

    #[test]
    fn resolve_xdg_paths_falls_back_to_tmp_with_uid() {
        let env = |k: &str| match k {
            "HOME" => Some("/home/u".to_string()),
            "UID" => Some("1000".into()),
            _ => None,
        };
        let (state, rt) = resolve_xdg_paths(env);
        assert_eq!(state, PathBuf::from("/home/u/.local/state/ark"));
        // No XDG_RUNTIME_DIR, UID present → /tmp/ark-1000/agents
        assert_eq!(rt, PathBuf::from("/tmp/ark-1000/agents"));
    }

    #[test]
    fn resolve_xdg_paths_empty_env_is_unset() {
        let env = |k: &str| match k {
            "XDG_STATE_HOME" => Some(String::new()),
            "HOME" => Some("/h".into()),
            "UID" => Some("42".into()),
            _ => None,
        };
        let (state, _rt) = resolve_xdg_paths(env);
        assert_eq!(state, PathBuf::from("/h/.local/state/ark"));
    }

    // ---- F-604: honor ARK_STATE_DIR + ARK_RUNTIME_DIR ----------------------

    #[test]
    fn resolve_xdg_paths_honors_ark_state_dir_over_xdg_and_home() {
        // ARK_STATE_DIR wins outright and is used verbatim (no `ark/`
        // suffix) — mirrors ark-types::EnvPaths::resolve.
        let env = |k: &str| match k {
            "ARK_STATE_DIR" => Some("/explicit/state".to_string()),
            "XDG_STATE_HOME" => Some("/xdg/state".to_string()),
            "HOME" => Some("/home/u".to_string()),
            "UID" => Some("1000".into()),
            _ => None,
        };
        let (state, _rt) = resolve_xdg_paths(env);
        assert_eq!(state, PathBuf::from("/explicit/state"));
    }

    #[test]
    fn resolve_xdg_paths_honors_ark_runtime_dir_over_xdg() {
        // ARK_RUNTIME_DIR wins outright: used verbatim + /agents appended
        // (no ark-$UID segment — caller chose an isolated path).
        let env = |k: &str| match k {
            "ARK_RUNTIME_DIR" => Some("/explicit/rt".to_string()),
            "XDG_RUNTIME_DIR" => Some("/run".to_string()),
            "UID" => Some("1000".into()),
            _ => None,
        };
        let (_state, rt) = resolve_xdg_paths(env);
        assert_eq!(rt, PathBuf::from("/explicit/rt/agents"));
    }

    #[test]
    fn resolve_xdg_paths_ark_runtime_dir_overrides_missing_uid() {
        // With ARK_RUNTIME_DIR set, UID absence is fine — no
        // disambiguation needed because the caller already picked an
        // explicit path.
        let env = |k: &str| match k {
            "ARK_RUNTIME_DIR" => Some("/explicit/rt".to_string()),
            _ => None,
        };
        let (_state, rt) = resolve_xdg_paths(env);
        assert_eq!(rt, PathBuf::from("/explicit/rt/agents"));
    }

    #[test]
    fn resolve_xdg_paths_returns_empty_runtime_when_uid_missing() {
        // F-604 documented rationale: without ARK_RUNTIME_DIR and without
        // a UID, we cannot build a per-user-isolated path safely
        // (`/tmp/ark/agents` would collide across users on a shared
        // host). Return an empty PathBuf so the caller skips the socket
        // scan; pipe liveness is still functional.
        //
        // F-609: the public `resolve_xdg_paths` now pulls a UID from
        // libc::geteuid() as a fallback, so to exercise the true
        // "no UID anywhere" branch we call the injectable variant
        // with a closure that returns `None`.
        let env = |k: &str| match k {
            "HOME" => Some("/home/u".to_string()),
            "XDG_RUNTIME_DIR" => Some("/run".to_string()),
            _ => None,
        };
        let (state, rt) = resolve_xdg_paths_with_uid(env, || None);
        assert_eq!(state, PathBuf::from("/home/u/.local/state/ark"));
        assert_eq!(rt, PathBuf::new());
    }

    // ---- F-609: UID-fallback closure seeds the socket path -----------------

    #[test]
    fn resolve_xdg_paths_uses_uid_fallback_when_env_lacks_uid() {
        // Zellij plugin harnesses frequently don't export `UID` — before
        // F-609 we'd return PathBuf::new() for runtime_dir and the socket
        // scan got skipped, so live agents were classified as crashed.
        // With the fallback closure the path is constructed cleanly.
        let env = |k: &str| match k {
            "HOME" => Some("/home/u".to_string()),
            "XDG_RUNTIME_DIR" => Some("/run".to_string()),
            _ => None,
        };
        let (_state, rt) = resolve_xdg_paths_with_uid(env, || Some("4242".to_string()));
        assert_eq!(rt, PathBuf::from("/run/ark-4242/agents"));
    }

    #[test]
    fn resolve_xdg_paths_env_uid_wins_over_fallback() {
        // Shell-convention env wins: if the user already exported UID,
        // honour it rather than poking geteuid() (useful for tests and
        // sandboxed run modes that re-map uids).
        let env = |k: &str| match k {
            "XDG_RUNTIME_DIR" => Some("/run".to_string()),
            "UID" => Some("1000".into()),
            _ => None,
        };
        let (_state, rt) = resolve_xdg_paths_with_uid(env, || Some("9999".to_string()));
        assert_eq!(rt, PathBuf::from("/run/ark-1000/agents"));
    }

    #[test]
    fn resolve_xdg_paths_fallback_empty_string_skips_runtime() {
        // Pathological fallback that yields an empty string must not
        // produce `ark-/agents` — treat it as "no uid available".
        let env = |_: &str| -> Option<String> { None };
        let (_state, rt) = resolve_xdg_paths_with_uid(env, || Some(String::new()));
        assert_eq!(rt, PathBuf::new());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_xdg_paths_default_uses_libc_geteuid_on_unix() {
        // Sanity: on unix hosts the production wrapper yields a non-empty
        // runtime path even when env is utterly empty — libc::geteuid
        // supplies the uid.
        let env = |_: &str| -> Option<String> { None };
        let (_state, rt) = resolve_xdg_paths(env);
        assert_ne!(rt, PathBuf::new(), "expected libc fallback to seed uid");
        let rt_str = rt.to_string_lossy();
        assert!(
            rt_str.starts_with("/tmp/ark-") && rt_str.ends_with("/agents"),
            "unexpected runtime path shape: {rt_str}"
        );
    }

    // ---- option D2: TMPDIR branch (macOS idiomatic fallback) ---------------

    #[test]
    fn resolve_xdg_paths_uses_tmpdir_without_uid_when_xdg_unset() {
        // macOS idiom: $TMPDIR is already per-user sandboxed
        // (/var/folders/…/T/), so no ark-$UID segment needed. Works even
        // when UID isn't recoverable.
        let env = |k: &str| match k {
            "TMPDIR" => Some("/var/folders/a/b/T/".to_string()),
            _ => None,
        };
        let (_state, rt) = resolve_xdg_paths_with_uid(env, || None);
        assert_eq!(rt, PathBuf::from("/var/folders/a/b/T/ark/agents"));
    }

    #[test]
    fn resolve_xdg_paths_xdg_wins_over_tmpdir() {
        let env = |k: &str| match k {
            "XDG_RUNTIME_DIR" => Some("/run".to_string()),
            "TMPDIR" => Some("/var/folders/a/b/T/".to_string()),
            "UID" => Some("1000".into()),
            _ => None,
        };
        let (_state, rt) = resolve_xdg_paths(env);
        assert_eq!(rt, PathBuf::from("/run/ark-1000/agents"));
    }

    #[test]
    fn resolve_xdg_paths_ark_runtime_dir_wins_over_tmpdir() {
        let env = |k: &str| match k {
            "ARK_RUNTIME_DIR" => Some("/explicit/rt".to_string()),
            "TMPDIR" => Some("/var/folders/a/b/T/".to_string()),
            _ => None,
        };
        let (_state, rt) = resolve_xdg_paths(env);
        assert_eq!(rt, PathBuf::from("/explicit/rt/agents"));
    }

    #[test]
    fn resolve_xdg_paths_empty_tmpdir_falls_through() {
        let env = |k: &str| match k {
            "TMPDIR" => Some(String::new()),
            "UID" => Some("501".into()),
            _ => None,
        };
        let (_state, rt) = resolve_xdg_paths(env);
        // Empty TMPDIR skipped → /tmp/ark-501/agents bare-Linux fallback.
        assert_eq!(rt, PathBuf::from("/tmp/ark-501/agents"));
    }

    #[test]
    fn resolve_xdg_paths_treats_empty_ark_vars_as_unset() {
        // Empty env strings (rare, but some shells export them) must
        // fall through to the next precedence tier.
        let env = |k: &str| match k {
            "ARK_STATE_DIR" => Some(String::new()),
            "ARK_RUNTIME_DIR" => Some(String::new()),
            "XDG_STATE_HOME" => Some("/xdg/state".to_string()),
            "XDG_RUNTIME_DIR" => Some("/run".to_string()),
            "UID" => Some("7".into()),
            _ => None,
        };
        let (state, rt) = resolve_xdg_paths(env);
        assert_eq!(state, PathBuf::from("/xdg/state/ark"));
        assert_eq!(rt, PathBuf::from("/run/ark-7/agents"));
    }
}
