//! `ark list` — T-023 (cavekit-soul Phase 1) + T-031 (Phase 2 host-dispatch R1).
//!
//! Enumerates sessions under `$STATE/sessions/*/` via
//! [`list_session_ids`], queries each supervisor over its
//! UnixStream control socket with `{"cmd":"Status"}`, and
//! renders either a compact table or a detail view.
//!
//! A missing / unresponsive socket does NOT fail the whole
//! command — the row is surfaced as an "orphan" so the user
//! can steer `ark doctor`.
//!
//! ## Columns
//!
//! Core columns (T-031 R1):
//!   Position 0: `id`
//!   Position 1: `name`
//!   Position 2: `status`
//! Additional core-rendered columns for human table mode
//! (`cwd`, `uptime`) trail the core triple to preserve the
//! pre-Phase-2 human-readable output exactly.
//!
//! Extension-contributed columns are APPENDED after all core
//! columns, sorted alpha by extension name then by the ext's
//! own declaration order. Columns are only collected from
//! extensions that advertise `ext.list_columns.v1` — gating
//! lives in the dispatcher, callers give us a flattened list.
//!
//! For v0.1-pre-T-030, no live extension dispatcher is wired;
//! callers pass `None` for the [`ExtensionColumnProvider`] and
//! output matches pre-Phase-2 behavior exactly. A future task
//! (see T-030 follow-up) wires a real provider that queries
//! loaded extensions over RPC.
//!
//! Methodology concepts (orchestrator, engine, phase, layout,
//! tab count, last event, findings, source) remain stripped
//! — those belong inside extensions now.
//!
//! Wire contract (from crates/supervisor/src/commands.rs):
//!
//!   Request:  {"cmd":"Status"}
//!   Response: {"ok":true,"data":<SessionStatus JSON>}
//!             {"ok":false,"error":"..."}

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ark_types::{SessionId, SessionStatus, StateLayout};
use clap::Args;
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::error::CliError;
use crate::id_resolver::{ResolveError, list_session_ids, resolve_session_id};

/// Arguments for `ark list`.
#[derive(Debug, Args)]
#[command(
    about = "List sessions (or show detail for one when [ID] is given)",
    long_about = "Show active and archived sessions. With ID, prints the\n\
                  detail view for that session.\n\
                  \n\
                  Examples:\n  \
                  ark list\n  \
                  ark list --watch\n  \
                  ark list myfeat\n  \
                  ark list myfeat --json"
)]
pub struct ListArgs {
    /// ID fragment (exact/prefix/substring). Shows detail if set.
    #[arg(value_name = "ID")]
    pub id: Option<String>,

    /// Emit a JSON array using the `SessionStatus` schema.
    #[arg(long)]
    pub json: bool,

    /// Re-render every 2s, clearing screen between.
    #[arg(long)]
    pub watch: bool,
}

/// A single session row.
///
/// - `Live`: socket answered with a `SessionStatus` — supervisor up.
/// - `Archived`: socket missing/dead BUT a persisted
///   `{state}/sessions/{id}/status.json` was found and parsed — the
///   supervisor wrote its final state at shutdown.
/// - `Orphan`: neither a socket response nor a parseable
///   `status.json` — genuinely abandoned, steer to `ark doctor`.
#[derive(Debug, Clone)]
enum Row {
    Live(SessionId, SessionStatus),
    Archived(SessionId, SessionStatus),
    Orphan(SessionId),
}

impl Row {
    fn id(&self) -> &SessionId {
        match self {
            Row::Live(id, _) | Row::Archived(id, _) | Row::Orphan(id) => id,
        }
    }

    fn name(&self) -> &str {
        self.id().name.as_str()
    }

    /// True when the supervisor answered the control socket.
    fn is_running(&self) -> bool {
        matches!(self, Row::Live(_, _))
    }

    /// Core status string for the `status` column (position 2 in
    /// the T-031 core column triple).
    #[allow(dead_code)]
    fn status_str(&self) -> &'static str {
        match self {
            Row::Live(_, _) => "running",
            Row::Archived(_, _) => "stopped",
            Row::Orphan(_) => "orphan",
        }
    }
}

// --- T-031: extension-column provider surface ---------------------
//
// `ark list` assembles its rendered column set by combining three
// groups, in order:
//
// 1. CORE fixed triple  — `id`, `name`, `status` at positions 0-2.
// 2. CORE extras        — `cwd`, `uptime` (human-table only, kept
//                         to preserve pre-Phase-2 output exactly).
// 3. EXTENSION columns  — appended after all core columns, sorted
//                         alpha by extension name, then by the
//                         extension's own declaration order (which
//                         is the insertion order into the Vec
//                         returned by `collect_columns()`).
//
// Callers pass an optional [`ExtensionColumnProvider`]. When None
// (the v0.1 default, and every `ark list` invocation until the
// T-030 load-sequence wires a real provider), only core columns
// render — human and JSON output match the pre-Phase-2 behavior.
//
// Capability gating is decided by the provider's implementation,
// which must only surface columns from extensions that advertise
// `ext.list_columns.v1` (see cavekit-soul-phase-2-host-dispatch
// R1 + supervisor::ext_dispatch::should_dispatch). A stub
// provider used in tests never touches the real dispatcher.

/// One extension-contributed column.
pub struct ExtensionColumn {
    /// Owning extension name (used for stable alpha-sorting).
    pub ext_name: String,
    /// Column header label.
    pub column_name: String,
    /// Resolver that renders a cell for the given session row.
    /// Returning `"-"` on missing data is convention.
    pub resolver: Box<dyn Fn(&SessionColumnCtx<'_>) -> String + Send + Sync>,
}

impl std::fmt::Debug for ExtensionColumn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionColumn")
            .field("ext_name", &self.ext_name)
            .field("column_name", &self.column_name)
            .field("resolver", &"<fn>")
            .finish()
    }
}

/// Resolver input — a read-only view of the session's core data
/// that an extension column needs to render a cell.
pub struct SessionColumnCtx<'a> {
    /// Session identifier.
    pub id: &'a SessionId,
    /// Latest supervisor-side status (None for Orphan rows).
    pub status: Option<&'a SessionStatus>,
    /// Whether the supervisor answered its control socket.
    pub running: bool,
}

/// Trait for collecting extension-contributed columns. Impls live
/// elsewhere (real impl: supervisor dispatcher client; test impl:
/// stub in the tests module). This module only consumes the trait.
pub trait ExtensionColumnProvider {
    /// Return the full, flattened list of extension columns. Order
    /// within the returned vec is preserved as the declaration
    /// order for columns belonging to the same extension; the
    /// consumer stable-sorts by `ext_name` after.
    ///
    /// Only extensions advertising `ext.list_columns.v1` should
    /// appear in the returned list — enforcement is the impl's
    /// responsibility.
    fn collect_columns(&self) -> Vec<ExtensionColumn>;
}

/// A resolved column ready to render — either a core column or an
/// extension contribution. Used by the column-assembly layer and
/// the assembly test.
pub struct ResolvedColumn {
    /// Group the column belongs to: `"core"` or the extension name.
    pub group: String,
    /// Column header.
    pub name: String,
    /// Resolver fn, boxed uniformly so core + ext columns compose.
    pub resolver: Box<dyn Fn(&SessionColumnCtx<'_>) -> String + Send + Sync>,
}

impl std::fmt::Debug for ResolvedColumn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedColumn")
            .field("group", &self.group)
            .field("name", &self.name)
            .field("resolver", &"<fn>")
            .finish()
    }
}

/// Assemble the full column list for the given provider. Core
/// columns are emitted first; extension columns follow, sorted
/// stable-alpha by extension name with declaration order
/// preserved within each extension.
///
/// `include_core_extras` controls whether `cwd` and `uptime` are
/// appended after the core triple (human-table mode wants them,
/// the pure-column-model assembly test does not).
pub fn assemble_columns(
    provider: Option<&dyn ExtensionColumnProvider>,
    include_core_extras: bool,
) -> Vec<ResolvedColumn> {
    let mut cols: Vec<ResolvedColumn> = Vec::new();

    // Position 0: id (short form, human-scannable).
    cols.push(ResolvedColumn {
        group: "core".into(),
        name: "id".into(),
        resolver: Box::new(|cx: &SessionColumnCtx<'_>| short_id(cx.id)),
    });
    // Position 1: name.
    cols.push(ResolvedColumn {
        group: "core".into(),
        name: "name".into(),
        resolver: Box::new(|cx: &SessionColumnCtx<'_>| cx.id.name.clone()),
    });
    // Position 2: status — one of "running"/"stopped"/"orphan".
    cols.push(ResolvedColumn {
        group: "core".into(),
        name: "status".into(),
        resolver: Box::new(|cx: &SessionColumnCtx<'_>| {
            if cx.running {
                "running".into()
            } else if cx.status.is_some() {
                "stopped".into()
            } else {
                "orphan".into()
            }
        }),
    });

    if include_core_extras {
        // Retained for pre-Phase-2 human-table compat (tests still
        // assert CWD / UPTIME / RUNNING? headers). Not part of the
        // kit's core-triple contract, but emitted before any ext
        // contribution.
        cols.push(ResolvedColumn {
            group: "core".into(),
            name: "cwd".into(),
            // Resolver is a placeholder — human table reads cwd
            // via spec.json in render_table_with_cwds. This field
            // exists so --json can include it uniformly.
            resolver: Box::new(|_cx: &SessionColumnCtx<'_>| "-".into()),
        });
        cols.push(ResolvedColumn {
            group: "core".into(),
            name: "uptime".into(),
            resolver: Box::new(|cx: &SessionColumnCtx<'_>| match cx.status {
                Some(s) => format_uptime_since(s.started_at),
                None => "-".into(),
            }),
        });
    }

    // Extension columns: collect, stable-sort by ext_name, append.
    // `sort_by` is stable in Rust → declaration order within each
    // extension is preserved (kit R1 acceptance criterion).
    if let Some(p) = provider {
        let mut ext_cols = p.collect_columns();
        ext_cols.sort_by(|a, b| a.ext_name.cmp(&b.ext_name));
        for c in ext_cols {
            cols.push(ResolvedColumn {
                group: c.ext_name,
                name: c.column_name,
                resolver: c.resolver,
            });
        }
    }

    cols
}

/// Read a persisted `status.json` for an archived session.
///
/// The supervisor writes its final `SessionStatus` atomically at
/// shutdown. When the control socket is gone (process exited) but
/// the file remains, we surface the session as `Row::Archived`
/// rather than misclassify it as orphaned.
///
/// Returns `None` on any of: file missing, unreadable, or
/// malformed JSON.
fn read_persisted_status(layout: &StateLayout, id: &SessionId) -> Option<SessionStatus> {
    let path = layout.session_status_path(id);
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice::<SessionStatus>(&bytes).ok()
}

/// Query one supervisor's status over its control socket.
/// Returns `Ok(None)` for any connect/IO error → caller
/// classifies as "orphan" rather than hard-failing.
fn query_status(sock: &std::path::Path) -> Option<SessionStatus> {
    let mut stream = UnixStream::connect(sock).ok()?;
    // Short timeouts so a wedged supervisor does not hang the
    // whole list invocation.
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();

    let req = json!({ "cmd": "Status" });
    let mut line = serde_json::to_vec(&req).ok()?;
    line.push(b'\n');
    stream.write_all(&line).ok()?;
    stream.flush().ok();

    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf).ok()?;
    let v: Value = serde_json::from_str(buf.trim()).ok()?;
    if !v.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let data = v.get("data")?.clone();
    serde_json::from_value::<SessionStatus>(data).ok()
}

/// Render a table into `out`. `no_color` suppresses ANSI.
fn render_table<W: Write>(out: &mut W, rows: &[Row], no_color: bool) -> std::io::Result<()> {
    // Columns (T-023): id, name, cwd, uptime, running?
    let hdr = ("ID", "NAME", "CWD", "UPTIME", "RUNNING?");
    let bold_on = if no_color { "" } else { "\x1b[1m" };
    let bold_off = if no_color { "" } else { "\x1b[0m" };
    writeln!(
        out,
        "{bold_on}{:<12}  {:<20}  {:<28}  {:>8}  {:<8}{bold_off}",
        hdr.0, hdr.1, hdr.2, hdr.3, hdr.4
    )?;
    if rows.is_empty() {
        writeln!(out, "(no sessions)")?;
        return Ok(());
    }
    for row in rows {
        let id_prefix = short_id(row.id());
        let name = truncate(row.name(), 20);
        let cwd = match row {
            Row::Live(_, _) | Row::Archived(_, _) => cwd_for(row),
            Row::Orphan(_) => "-".to_string(),
        };
        let cwd = truncate(&cwd, 28);
        let uptime = match row {
            Row::Live(_, s) | Row::Archived(_, s) => format_uptime_since(s.started_at),
            Row::Orphan(_) => "-".to_string(),
        };
        let running = if row.is_running() { "yes" } else { "no" };
        writeln!(
            out,
            "{:<12}  {:<20}  {:<28}  {:>8}  {:<8}",
            id_prefix, name, cwd, uptime, running,
        )?;
    }
    Ok(())
}

/// CWD is not on `SessionStatus` itself — it lives on `SessionSpec`,
/// which we can fetch from `spec.json` alongside the status. For
/// rows surfaced via the live socket, the supervisor does not ship
/// cwd in the status payload, so we fall back to reading `spec.json`
/// off disk. Errors collapse to `"-"`.
fn cwd_for(_row: &Row) -> String {
    // Placeholder — cwd is read via `spec.json` in gather_rows and
    // attached to the row. We avoid touching the filesystem here to
    // keep the render function pure and cheap for `--watch`.
    "-".to_string()
}

/// First 12 chars of the session id (stable, human-scannable).
fn short_id(id: &SessionId) -> String {
    let s = id.as_str();
    if s.len() <= 12 {
        s
    } else {
        s.chars().take(12).collect()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Format duration since `started_at` as a compact string:
/// `42s`, `7m`, `3h`, `2d`.
fn format_uptime_since(started_at: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(started_at);
    let secs = delta.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Render the detail view for a single session.
fn render_detail<W: Write>(
    out: &mut W,
    row: &Row,
    cwd: Option<&std::path::Path>,
    _no_color: bool,
) -> std::io::Result<()> {
    match row {
        Row::Live(id, s) | Row::Archived(id, s) => {
            writeln!(out, "id:       {}", id.as_str())?;
            writeln!(out, "name:     {}", id.name)?;
            if let Some(cwd) = cwd {
                writeln!(out, "cwd:      {}", cwd.display())?;
            } else {
                writeln!(out, "cwd:      -")?;
            }
            writeln!(out, "uptime:   {}", format_uptime_since(s.started_at))?;
            writeln!(
                out,
                "running?: {}",
                if row.is_running() { "yes" } else { "no" }
            )?;
            Ok(())
        }
        Row::Orphan(id) => {
            writeln!(out, "id:       {}", id.as_str())?;
            writeln!(out, "name:     {}", id.name)?;
            writeln!(out, "cwd:      -")?;
            writeln!(out, "uptime:   -")?;
            writeln!(
                out,
                "running?: orphan (no live supervisor; try `ark doctor`)"
            )?;
            Ok(())
        }
    }
}

/// Map a [`ResolveError`] to the appropriate [`CliError`].
fn map_resolve_err(e: ResolveError, query: &str) -> CliError {
    match e {
        ResolveError::NotFound { .. } => CliError::NotFound {
            what: query.to_string(),
        },
        ResolveError::AmbiguousPrefix { candidates, .. }
        | ResolveError::AmbiguousSubstring { candidates, .. }
        | ResolveError::AmbiguousName { candidates, .. } => CliError::Ambiguous {
            what: query.to_string(),
            candidates: candidates.into_iter().map(|c| c.as_str()).collect(),
        },
        ResolveError::Io(err) => CliError::Generic {
            reason: format!("resolve: {err}"),
        },
    }
}

/// Read the `cwd` out of `$base/sessions/{id}/spec.json`. Returns
/// `None` on any error (missing, unreadable, malformed JSON, missing
/// `cwd` field).
fn read_spec_cwd(layout: &StateLayout, id: &SessionId) -> Option<std::path::PathBuf> {
    #[derive(serde::Deserialize)]
    struct SpecCwd {
        cwd: std::path::PathBuf,
    }
    let path = layout.session_spec_path(id);
    let bytes = std::fs::read(&path).ok()?;
    let proj: SpecCwd = serde_json::from_slice(&bytes).ok()?;
    Some(proj.cwd)
}

/// Build the row-set for either all sessions or a single one.
///
/// A genuine IO failure from `list_session_ids` (e.g. unreadable
/// `sessions_root`) is surfaced as `CliError::Generic`, not silently
/// swallowed into an empty list. Missing `sessions_root` is already
/// treated as empty inside `list_session_ids` itself.
fn gather_rows(layout: &StateLayout, only: Option<&SessionId>) -> Result<Vec<Row>, CliError> {
    let ids: Vec<SessionId> = match only {
        Some(id) => vec![id.clone()],
        None => list_session_ids(layout).map_err(|err| CliError::Generic {
            reason: format!("read sessions_root: {err}"),
        })?,
    };
    Ok(ids
        .into_iter()
        .map(|id| {
            let sock = layout.session_socket_path(&id);
            match query_status(&sock) {
                Some(status) => Row::Live(id, status),
                None => match read_persisted_status(layout, &id) {
                    Some(status) => Row::Archived(id, status),
                    None => Row::Orphan(id),
                },
            }
        })
        .collect())
}

/// Non-watch dispatch path — render once and return.
fn run_once(args: &ListArgs, ctx: &Ctx) -> Result<(), CliError> {
    let layout = StateLayout::new(
        ctx.state_dir.clone(),
        ctx.runtime_dir.clone(),
        ctx.config_dir.clone(),
    );

    // ID branch: resolve + single-session query.
    if let Some(query) = args.id.as_deref() {
        let resolved = resolve_session_id(query, &layout).map_err(|e| map_resolve_err(e, query))?;
        let rows = gather_rows(&layout, Some(&resolved))?;
        let stdout = std::io::stdout();
        let mut h = stdout.lock();
        if args.json {
            emit_json(&mut h, &rows, true, &layout)?;
        } else if let Some(row) = rows.first() {
            let cwd = read_spec_cwd(&layout, row.id());
            render_detail(&mut h, row, cwd.as_deref(), ctx.no_color).map_err(io_to_cli)?;
        } else {
            writeln!(h, "(no sessions match)").map_err(io_to_cli)?;
        }
        return Ok(());
    }

    // List branch: enumerate all.
    let rows = gather_rows(&layout, None)?;
    let stdout = std::io::stdout();
    let mut h = stdout.lock();
    if args.json {
        emit_json(&mut h, &rows, false, &layout)?;
    } else {
        render_table_with_cwds(&mut h, &rows, &layout, ctx.no_color).map_err(io_to_cli)?;
    }
    Ok(())
}

/// Table variant that eagerly pulls cwd per row from `spec.json`. The
/// bare [`render_table`] is kept as a pure function for tests that
/// don't want to write a spec.json.
fn render_table_with_cwds<W: Write>(
    out: &mut W,
    rows: &[Row],
    layout: &StateLayout,
    no_color: bool,
) -> std::io::Result<()> {
    let hdr = ("ID", "NAME", "CWD", "UPTIME", "RUNNING?");
    let bold_on = if no_color { "" } else { "\x1b[1m" };
    let bold_off = if no_color { "" } else { "\x1b[0m" };
    writeln!(
        out,
        "{bold_on}{:<12}  {:<20}  {:<28}  {:>8}  {:<8}{bold_off}",
        hdr.0, hdr.1, hdr.2, hdr.3, hdr.4
    )?;
    if rows.is_empty() {
        writeln!(out, "(no sessions)")?;
        return Ok(());
    }
    for row in rows {
        let id_prefix = short_id(row.id());
        let name = truncate(row.name(), 20);
        let cwd_str = read_spec_cwd(layout, row.id())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        let cwd = truncate(&cwd_str, 28);
        let uptime = match row {
            Row::Live(_, s) | Row::Archived(_, s) => format_uptime_since(s.started_at),
            Row::Orphan(_) => "-".to_string(),
        };
        let running = if row.is_running() { "yes" } else { "no" };
        writeln!(
            out,
            "{:<12}  {:<20}  {:<28}  {:>8}  {:<8}",
            id_prefix, name, cwd, uptime, running,
        )?;
    }
    Ok(())
}

fn io_to_cli(e: std::io::Error) -> CliError {
    CliError::Generic {
        reason: format!("write: {e}"),
    }
}

/// Emit JSON: array for list mode, single object for detail.
fn emit_json<W: Write>(
    out: &mut W,
    rows: &[Row],
    single: bool,
    layout: &StateLayout,
) -> Result<(), CliError> {
    let values: Vec<Value> = rows
        .iter()
        .map(|r| match r {
            Row::Live(id, s) => {
                let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
                if let Value::Object(ref mut m) = v {
                    m.insert("running".to_string(), Value::Bool(true));
                    if let Some(cwd) = read_spec_cwd(layout, id) {
                        m.insert("cwd".to_string(), Value::String(cwd.display().to_string()));
                    }
                }
                v
            }
            Row::Archived(id, s) => {
                let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
                if let Value::Object(ref mut m) = v {
                    m.insert("running".to_string(), Value::Bool(false));
                    if let Some(cwd) = read_spec_cwd(layout, id) {
                        m.insert("cwd".to_string(), Value::String(cwd.display().to_string()));
                    }
                }
                v
            }
            Row::Orphan(id) => json!({
                "id": id,
                "running": false,
                "orphan": true,
            }),
        })
        .collect();
    let payload = if single {
        values.into_iter().next().unwrap_or(Value::Null)
    } else {
        Value::Array(values)
    };
    let s = serde_json::to_string_pretty(&payload).map_err(|e| CliError::Generic {
        reason: format!("encode json: {e}"),
    })?;
    writeln!(out, "{s}").map_err(io_to_cli)?;
    Ok(())
}

/// Dispatch `ark list`. `--watch` loops `run_once` every 2s with an
/// ANSI clear-screen between frames; Ctrl-C terminates the loop
/// (handled by the shell / OS).
pub fn run(args: ListArgs, ctx: &Ctx) -> Result<(), CliError> {
    if args.watch && !args.json {
        loop {
            if !ctx.no_color {
                print!("\x1b[2J\x1b[H");
                std::io::stdout().flush().ok();
            }
            run_once(&args, ctx)?;
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    run_once(&args, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_types::SessionStatus;
    use clap::Parser;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: ListArgs,
    }

    // ---------- parse round-trip ----------

    #[test]
    fn bare_list_has_no_id() {
        let h = Host::try_parse_from(["list"]).expect("parse");
        assert!(h.args.id.is_none());
        assert!(!h.args.json);
        assert!(!h.args.watch);
    }

    #[test]
    fn id_positional_parses() {
        let h = Host::try_parse_from(["list", "myfeat"]).expect("parse");
        assert_eq!(h.args.id.as_deref(), Some("myfeat"));
    }

    #[test]
    fn watch_flag_parses() {
        let h = Host::try_parse_from(["list", "--watch"]).expect("parse");
        assert!(h.args.watch);
    }

    #[test]
    fn json_flag_parses() {
        let h = Host::try_parse_from(["list", "--json"]).expect("parse");
        assert!(h.args.json);
    }

    #[test]
    fn orchestrator_flag_rejected() {
        // Post-T-023 the --orchestrator flag is gone.
        let res = Host::try_parse_from(["list", "--orchestrator", "cavekit"]);
        assert!(res.is_err(), "--orchestrator must no longer parse");
    }

    #[test]
    fn status_flag_rejected() {
        // Post-T-023 the --status flag is gone.
        let res = Host::try_parse_from(["list", "--status", "running"]);
        assert!(res.is_err(), "--status must no longer parse");
    }

    // ---------- helpers ----------

    fn layout_ctx(base: PathBuf) -> Ctx {
        Ctx {
            no_color: false,
            log_level: "info".into(),
            state_dir: base.clone(),
            config_dir: base.join("cfg"),
            runtime_dir: base.join("rt"),
        }
    }

    fn mk_status(id: &SessionId) -> SessionStatus {
        SessionStatus {
            id: id.clone(),
            started_at: chrono::Utc::now(),
            terminated_at: None,
            ext_state: BTreeMap::new(),
        }
    }

    fn seed_session(layout: &StateLayout, id: &SessionId) {
        fs::create_dir_all(layout.session_dir(id)).expect("mkdir");
    }

    // ---------- table rendering ----------

    #[test]
    fn table_renders_header_and_rows_no_color() {
        let id = SessionId::new("auth");
        let rows = vec![Row::Live(id.clone(), mk_status(&id))];
        let mut buf: Vec<u8> = Vec::new();
        render_table(&mut buf, &rows, true).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("ID"));
        assert!(s.contains("NAME"));
        assert!(s.contains("RUNNING?"));
        assert!(s.contains("auth"));
        assert!(s.contains("yes"));
        assert!(!s.contains("\x1b["), "unexpected ANSI: {s:?}");
    }

    #[test]
    fn table_uses_ansi_bold_when_color_allowed() {
        let rows: Vec<Row> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        render_table(&mut buf, &rows, false).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("\x1b[1m"), "expected ANSI bold: {s:?}");
    }

    #[test]
    fn table_empty_shows_no_sessions_marker() {
        let rows: Vec<Row> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        render_table(&mut buf, &rows, true).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("(no sessions)"));
    }

    #[test]
    fn table_drops_orchestrator_and_phase_columns() {
        // Post-T-023 header must not contain stripped columns.
        let rows: Vec<Row> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        render_table(&mut buf, &rows, true).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(!s.contains("ORCH"), "ORCH column must be gone");
        assert!(!s.contains("PHASE"), "PHASE column must be gone");
        assert!(!s.contains("FINDINGS"), "FINDINGS column must be gone");
    }

    // ---------- JSON shape ----------

    #[test]
    fn json_emits_array_for_list_mode() {
        let tmp = tempdir().expect("tempdir");
        let layout = StateLayout::new(
            tmp.path().to_path_buf(),
            tmp.path().join("rt"),
            tmp.path().join("cfg"),
        );
        let id = SessionId::new("auth");
        let rows = vec![Row::Live(id.clone(), mk_status(&id))];
        let mut buf: Vec<u8> = Vec::new();
        emit_json(&mut buf, &rows, false, &layout).expect("emit");
        let s = String::from_utf8(buf).expect("utf8");
        let v: Value = serde_json::from_str(&s).expect("json");
        assert!(v.is_array(), "expected array, got {s}");
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["running"], true);
    }

    #[test]
    fn json_single_mode_emits_object() {
        let tmp = tempdir().expect("tempdir");
        let layout = StateLayout::new(
            tmp.path().to_path_buf(),
            tmp.path().join("rt"),
            tmp.path().join("cfg"),
        );
        let id = SessionId::new("auth");
        let rows = vec![Row::Live(id.clone(), mk_status(&id))];
        let mut buf: Vec<u8> = Vec::new();
        emit_json(&mut buf, &rows, true, &layout).expect("emit");
        let s = String::from_utf8(buf).expect("utf8");
        let v: Value = serde_json::from_str(&s).expect("json");
        assert!(v.is_object(), "expected object, got {s}");
        assert_eq!(v["running"], true);
    }

    #[test]
    fn json_orphan_row_emits_minimal_shape() {
        let tmp = tempdir().expect("tempdir");
        let layout = StateLayout::new(
            tmp.path().to_path_buf(),
            tmp.path().join("rt"),
            tmp.path().join("cfg"),
        );
        let id = SessionId::new("dead");
        let rows = vec![Row::Orphan(id.clone())];
        let mut buf: Vec<u8> = Vec::new();
        emit_json(&mut buf, &rows, false, &layout).expect("emit");
        let v: Value = serde_json::from_slice(&buf).expect("json");
        let arr = v.as_array().expect("array");
        assert_eq!(arr[0]["running"], false);
        assert_eq!(arr[0]["orphan"], true);
    }

    // ---------- resolver error mapping ----------

    #[test]
    fn not_found_id_returns_not_found_err() {
        let tmp = tempdir().expect("tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let args = ListArgs {
            id: Some("ghost".into()),
            json: false,
            watch: false,
        };
        let err = run_once(&args, &ctx).expect_err("should be not-found");
        assert!(matches!(err, CliError::NotFound { .. }));
    }

    // ---------- missing socket → orphan row ----------

    #[test]
    fn missing_socket_yields_orphan_row_in_table_mode() {
        let tmp = tempfile::Builder::new()
            .prefix("arklist")
            .tempdir_in("/tmp")
            .expect("short tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = SessionId::new("dead");
        seed_session(&layout, &id);

        let rows = gather_rows(&layout, None).expect("gather");
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], Row::Orphan(_)));

        let mut buf: Vec<u8> = Vec::new();
        render_table(&mut buf, &rows, true).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("dead"));
        assert!(s.contains("no"));
    }

    // ---------- detail view ----------

    #[test]
    fn detail_view_prints_core_fields() {
        let id = SessionId::new("auth");
        let row = Row::Live(id.clone(), mk_status(&id));
        let mut buf: Vec<u8> = Vec::new();
        render_detail(
            &mut buf,
            &row,
            Some(std::path::Path::new("/tmp/work")),
            true,
        )
        .expect("detail");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("id:"));
        assert!(s.contains("name:"));
        assert!(s.contains("cwd:"));
        assert!(s.contains("/tmp/work"));
        assert!(s.contains("uptime:"));
        assert!(s.contains("running?:"));
    }

    #[test]
    fn detail_view_omits_stripped_fields() {
        let id = SessionId::new("auth");
        let row = Row::Live(id.clone(), mk_status(&id));
        let mut buf: Vec<u8> = Vec::new();
        render_detail(&mut buf, &row, None, true).expect("detail");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(!s.contains("orchestrator:"));
        assert!(!s.contains("engine:"));
        assert!(!s.contains("phase:"));
        assert!(!s.contains("layout:"));
        assert!(!s.contains("tab count:"));
        assert!(!s.contains("last event:"));
        assert!(!s.contains("findings:"));
    }

    // ---------- watch flag semantics ----------

    #[test]
    fn watch_with_json_falls_through_to_run_once() {
        let tmp = tempdir().expect("tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let args = ListArgs {
            id: None,
            json: true,
            watch: true,
        };
        run(args, &ctx).expect("should not loop under --json");
    }

    #[test]
    fn short_id_takes_first_twelve_chars() {
        let id = SessionId::new("foo");
        let s = short_id(&id);
        assert_eq!(s.chars().count(), 12);
    }

    #[test]
    fn format_uptime_compact_under_minute() {
        let now = chrono::Utc::now();
        assert_eq!(format_uptime_since(now), "0s");
    }

    // ---------- T-031 extension column provider ----------

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Stub provider that returns a fixed set of columns. `calls`
    /// counts `collect_columns` invocations so tests can verify
    /// capability-gated skipping.
    struct StubProvider {
        cols: Vec<(String, String, String)>, // (ext, col, literal cell value)
        calls: Arc<AtomicUsize>,
    }

    impl ExtensionColumnProvider for StubProvider {
        fn collect_columns(&self) -> Vec<ExtensionColumn> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.cols
                .iter()
                .map(|(ext, col, literal)| {
                    let lit = literal.clone();
                    ExtensionColumn {
                        ext_name: ext.clone(),
                        column_name: col.clone(),
                        resolver: Box::new(move |_cx| lit.clone()),
                    }
                })
                .collect()
        }
    }

    #[test]
    fn list_columns_order_core_then_ext_alpha() {
        // ext-b declared first, ext-a second. Final order must
        // be ext-a.c1, ext-a.c2, ext-b.c1, ext-b.c2 (alpha by
        // ext, declaration-order within ext).
        let provider = StubProvider {
            cols: vec![
                ("ext-b".into(), "c1".into(), "b1".into()),
                ("ext-b".into(), "c2".into(), "b2".into()),
                ("ext-a".into(), "c1".into(), "a1".into()),
                ("ext-a".into(), "c2".into(), "a2".into()),
            ],
            calls: Arc::new(AtomicUsize::new(0)),
        };

        let cols = assemble_columns(Some(&provider), false);
        // Core triple first.
        assert_eq!(cols[0].group, "core");
        assert_eq!(cols[0].name, "id");
        assert_eq!(cols[1].group, "core");
        assert_eq!(cols[1].name, "name");
        assert_eq!(cols[2].group, "core");
        assert_eq!(cols[2].name, "status");
        // Then ext columns alpha-sorted then declaration order.
        assert_eq!(cols[3].group, "ext-a");
        assert_eq!(cols[3].name, "c1");
        assert_eq!(cols[4].group, "ext-a");
        assert_eq!(cols[4].name, "c2");
        assert_eq!(cols[5].group, "ext-b");
        assert_eq!(cols[5].name, "c1");
        assert_eq!(cols[6].group, "ext-b");
        assert_eq!(cols[6].name, "c2");
        assert_eq!(cols.len(), 7);
    }

    #[test]
    fn no_provider_renders_core_triple_only() {
        let cols = assemble_columns(None, false);
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0].name, "id");
        assert_eq!(cols[1].name, "name");
        assert_eq!(cols[2].name, "status");
        assert!(cols.iter().all(|c| c.group == "core"));
    }

    #[test]
    fn core_extras_appended_after_triple_and_before_ext() {
        let provider = StubProvider {
            cols: vec![("ext-z".into(), "cz".into(), "zv".into())],
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let cols = assemble_columns(Some(&provider), true);
        // [id, name, status, cwd, uptime, ext-z.cz]
        assert_eq!(cols.len(), 6);
        assert_eq!(cols[0].name, "id");
        assert_eq!(cols[1].name, "name");
        assert_eq!(cols[2].name, "status");
        assert_eq!(cols[3].name, "cwd");
        assert_eq!(cols[3].group, "core");
        assert_eq!(cols[4].name, "uptime");
        assert_eq!(cols[4].group, "core");
        assert_eq!(cols[5].group, "ext-z");
        assert_eq!(cols[5].name, "cz");
    }

    #[test]
    fn core_status_column_resolves_running_stopped_orphan() {
        let id = SessionId::new("s");
        let status = mk_status(&id);

        let cols = assemble_columns(None, false);
        let status_col = &cols[2];

        let live_cx = SessionColumnCtx {
            id: &id,
            status: Some(&status),
            running: true,
        };
        assert_eq!((status_col.resolver)(&live_cx), "running");

        let archived_cx = SessionColumnCtx {
            id: &id,
            status: Some(&status),
            running: false,
        };
        assert_eq!((status_col.resolver)(&archived_cx), "stopped");

        let orphan_cx = SessionColumnCtx {
            id: &id,
            status: None,
            running: false,
        };
        assert_eq!((status_col.resolver)(&orphan_cx), "orphan");
    }

    #[test]
    fn ext_column_resolver_invoked_per_row() {
        // Smoke: a resolver that formats on the row data should
        // produce the expected cell for a Live row.
        let id = SessionId::new("s1");
        let status = mk_status(&id);
        let cx = SessionColumnCtx {
            id: &id,
            status: Some(&status),
            running: true,
        };
        let col = ExtensionColumn {
            ext_name: "ext-x".into(),
            column_name: "badge".into(),
            resolver: Box::new(|cx: &SessionColumnCtx<'_>| {
                format!("{}-{}", cx.id.name, if cx.running { "up" } else { "down" })
            }),
        };
        assert_eq!((col.resolver)(&cx), "s1-up");
    }

    #[test]
    fn row_status_str_matches_variant() {
        let id = SessionId::new("s");
        assert_eq!(
            Row::Live(id.clone(), mk_status(&id)).status_str(),
            "running"
        );
        assert_eq!(
            Row::Archived(id.clone(), mk_status(&id)).status_str(),
            "stopped"
        );
        assert_eq!(Row::Orphan(id).status_str(), "orphan");
    }
}
