//! `ark list` — T-088 (cavekit-cli R3).
//!
//! Enumerates agents under `$STATE/agents/*/` via
//! [`list_agent_ids`], queries each supervisor over its
//! UnixStream control socket with `{"cmd":"Status"}`, and
//! renders either a compact table or a detail view.
//!
//! A missing / unresponsive socket does NOT fail the whole
//! command — the row is surfaced as an "orphan" so the user
//! can steer `ark doctor`.
//!
//! Wire contract (from crates/supervisor/src/commands.rs):
//!
//!   Request:  {"cmd":"Status"}
//!   Response: {"ok":true,"data":<AgentStatus JSON>}
//!             {"ok":false,"error":"..."}
//!
//! We import `AgentStatus` from `ark-types` directly — it is
//! `pub` there, so no on-wire mirror struct is needed.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ark_types::{AgentId, AgentStatus, Phase, StateLayout};
use clap::Args;
use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::error::CliError;
use crate::id_resolver::{ResolveError, list_agent_ids, resolve_agent_id};

/// Arguments for `ark list`.
#[derive(Debug, Args)]
#[command(
    about = "List agents (or show detail for one when [ID] is given)",
    long_about = "Show active and archived agents. With ID, prints the\n\
                  detail view for that agent (what `ark status` would\n\
                  have shown).\n\
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

    /// Filter by orchestrator.
    #[arg(long)]
    pub orchestrator: Option<String>,

    /// Filter by lifecycle status.
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,

    /// Emit a JSON array using the `AgentStatus` schema.
    #[arg(long)]
    pub json: bool,

    /// Re-render every 2s, clearing screen between.
    #[arg(long)]
    pub watch: bool,
}

/// A single agent row.
///
/// - `Live`: socket answered with an `AgentStatus` — supervisor up.
/// - `Archived`: socket missing/dead BUT a persisted
///   `{state}/agents/{id}/status.json` was found and parsed — the
///   supervisor wrote its final state at shutdown (F-518).
/// - `Orphan`: neither a socket response nor a parseable
///   `status.json` — genuinely abandoned, steer to `ark doctor`.
#[derive(Debug, Clone)]
enum Row {
    Live(AgentId, AgentStatus),
    Archived(AgentId, AgentStatus),
    Orphan(AgentId),
}

impl Row {
    fn id(&self) -> &AgentId {
        match self {
            Row::Live(id, _) | Row::Archived(id, _) | Row::Orphan(id) => id,
        }
    }

    fn phase_str(&self) -> &'static str {
        match self {
            Row::Live(_, s) | Row::Archived(_, s) => phase_name(s.phase),
            Row::Orphan(_) => "orphan",
        }
    }

    fn orchestrator(&self) -> &str {
        match self {
            Row::Live(_, s) | Row::Archived(_, s) => s.spec.orchestrator.as_str(),
            Row::Orphan(id) => id.orchestrator(),
        }
    }

    fn name(&self) -> &str {
        match self {
            Row::Live(_, s) | Row::Archived(_, s) => s.spec.name.as_str(),
            Row::Orphan(id) => id.name(),
        }
    }
}

/// Canonical lowercase phase name (matches serde snake_case).
fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Starting => "starting",
        Phase::Running => "running",
        Phase::Idle => "idle",
        Phase::Prompting => "prompting",
        Phase::Reviewing => "reviewing",
        Phase::Done => "done",
        Phase::Failed => "failed",
        Phase::Crashed => "crashed",
        Phase::Killed => "killed",
        Phase::Timeout => "timeout",
    }
}

/// Known phase names accepted by `--status`.
const PHASE_NAMES: &[&str] = &[
    "starting",
    "running",
    "idle",
    "prompting",
    "reviewing",
    "done",
    "failed",
    "crashed",
    "killed",
    "timeout",
];

fn is_known_phase(s: &str) -> bool {
    PHASE_NAMES.contains(&s)
}

/// Read a persisted `status.json` for an archived agent (F-518).
///
/// The supervisor writes its final `AgentStatus` atomically at
/// shutdown (see `crates/core/src/status_writer.rs`). When the
/// control socket is gone (process exited) but the file remains,
/// we surface the agent as `Row::Archived` rather than misclassify
/// it as orphaned.
///
/// Returns `None` on any of: file missing, unreadable, or
/// malformed JSON. The caller treats "no status.json" as a signal
/// to emit `Row::Orphan`.
fn read_persisted_status(layout: &StateLayout, id: &AgentId) -> Option<AgentStatus> {
    let path = layout.status_path(id);
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice::<AgentStatus>(&bytes).ok()
}

/// Query one supervisor's status over its control socket.
/// Returns `Ok(None)` for any connect/IO error → caller
/// classifies as "orphan" rather than hard-failing.
fn query_status(sock: &std::path::Path) -> Option<AgentStatus> {
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
    serde_json::from_value::<AgentStatus>(data).ok()
}

/// Apply `--status` / `--orchestrator` filters to the rows.
fn filter_rows(rows: Vec<Row>, args: &ListArgs) -> Vec<Row> {
    rows.into_iter()
        .filter(|r| match &args.status {
            Some(s) => r.phase_str() == s,
            None => true,
        })
        .filter(|r| match &args.orchestrator {
            Some(o) => r.orchestrator() == o,
            None => true,
        })
        .collect()
}

/// Render a table into `out`. `no_color` suppresses ANSI.
fn render_table<W: Write>(out: &mut W, rows: &[Row], no_color: bool) -> std::io::Result<()> {
    // Columns: id-prefix, name, orchestrator, phase, uptime.
    // Widths are fixed-small — ark list is meant to be scannable.
    let hdr = ("ID", "NAME", "ORCH", "PHASE", "UPTIME");
    let bold_on = if no_color { "" } else { "\x1b[1m" };
    let bold_off = if no_color { "" } else { "\x1b[0m" };
    writeln!(
        out,
        "{bold_on}{:<12}  {:<20}  {:<12}  {:<10}  {:>8}{bold_off}",
        hdr.0, hdr.1, hdr.2, hdr.3, hdr.4
    )?;
    if rows.is_empty() {
        writeln!(out, "(no agents)")?;
        return Ok(());
    }
    for row in rows {
        let id_prefix = short_id(row.id());
        let name = truncate(row.name(), 20);
        let orch = truncate(row.orchestrator(), 12);
        let phase = row.phase_str();
        let uptime = match row {
            Row::Live(_, s) | Row::Archived(_, s) => format_uptime_since(s.spec.created_at),
            Row::Orphan(_) => "-".to_string(),
        };
        writeln!(
            out,
            "{:<12}  {:<20}  {:<12}  {:<10}  {:>8}",
            id_prefix, name, orch, phase, uptime,
        )?;
    }
    Ok(())
}

/// First 12 chars of the agent id (stable, human-scannable).
fn short_id(id: &AgentId) -> String {
    let s = id.as_str();
    if s.len() <= 12 {
        s.to_string()
    } else {
        s.chars().take(12).collect()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Format duration since `created_at` as a compact string:
/// `42s`, `7m`, `3h`, `2d`.
fn format_uptime_since(created_at: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(created_at);
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

/// Render the detail view for a single agent.
fn render_detail<W: Write>(out: &mut W, row: &Row, _no_color: bool) -> std::io::Result<()> {
    match row {
        Row::Live(_, s) | Row::Archived(_, s) => {
            writeln!(out, "id:           {}", s.spec.id.as_str())?;
            writeln!(out, "name:         {}", s.spec.name)?;
            writeln!(out, "cwd:          {}", s.spec.cwd.display())?;
            writeln!(out, "orchestrator: {}", s.spec.orchestrator)?;
            writeln!(out, "engine:       {}", s.spec.engine)?;
            writeln!(out, "phase:        {}", phase_name(s.phase))?;
            writeln!(
                out,
                "uptime:       {}",
                format_uptime_since(s.spec.created_at)
            )?;
            let layout = s.spec.layout.as_deref().unwrap_or("(default)");
            writeln!(out, "layout:       {}", layout)?;
            writeln!(out, "tab count:    {}", s.tab_handles.len())?;
            writeln!(out, "last event:   {}", s.last_event_summary)?;
            if matches!(row, Row::Archived(_, _)) {
                writeln!(out, "source:       status.json (supervisor archived)")?;
            }
            Ok(())
        }
        Row::Orphan(id) => {
            writeln!(out, "id:           {}", id.as_str())?;
            writeln!(out, "phase:        orphan")?;
            writeln!(out, "note:         no live supervisor; try `ark doctor`")?;
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
            candidates: candidates
                .into_iter()
                .map(|c| c.as_str().to_string())
                .collect(),
        },
        ResolveError::Io(err) => CliError::Generic {
            reason: format!("resolve: {err}"),
        },
    }
}

/// Build the row-set for either all agents or a single one.
///
/// F-505: a genuine IO failure from `list_agent_ids` (e.g. unreadable
/// `agents_root`) is surfaced as `CliError::Generic`, not silently
/// swallowed into an empty list. Missing `agents_root` is already
/// treated as empty inside `list_agent_ids` itself.
fn gather_rows(layout: &StateLayout, only: Option<&AgentId>) -> Result<Vec<Row>, CliError> {
    let ids: Vec<AgentId> = match only {
        Some(id) => vec![id.clone()],
        None => list_agent_ids(layout).map_err(|err| CliError::Generic {
            reason: format!("read agents_root: {err}"),
        })?,
    };
    Ok(ids
        .into_iter()
        .map(|id| {
            let sock = layout.agent_socket_path(&id);
            match query_status(&sock) {
                Some(status) => Row::Live(id, status),
                None => match read_persisted_status(layout, &id) {
                    // F-518: supervisor is gone but its final
                    // `status.json` remains — surface the archived
                    // phase instead of classifying as orphan.
                    Some(status) => Row::Archived(id, status),
                    None => Row::Orphan(id),
                },
            }
        })
        .collect())
}

/// Non-watch dispatch path — render once and return.
fn run_once(args: &ListArgs, ctx: &Ctx) -> Result<(), CliError> {
    if let Some(ref status_filter) = args.status
        && !is_known_phase(status_filter)
    {
        return Err(CliError::Generic {
            reason: format!(
                "unknown --status '{status_filter}' (want one of: {})",
                PHASE_NAMES.join(", ")
            ),
        });
    }

    let layout = StateLayout::new(
        ctx.state_dir.clone(),
        ctx.runtime_dir.clone(),
        ctx.config_dir.clone(),
    );

    // ID branch: resolve + single-agent query.
    if let Some(query) = args.id.as_deref() {
        let resolved = resolve_agent_id(query, &layout).map_err(|e| map_resolve_err(e, query))?;
        let rows = gather_rows(&layout, Some(&resolved))?;
        let rows = filter_rows(rows, args);
        let stdout = std::io::stdout();
        let mut h = stdout.lock();
        if args.json {
            emit_json(&mut h, &rows, true)?;
        } else if let Some(row) = rows.first() {
            render_detail(&mut h, row, ctx.no_color).map_err(io_to_cli)?;
        } else {
            // Filter excluded the single resolved agent.
            writeln!(h, "(no agents match filters)").map_err(io_to_cli)?;
        }
        return Ok(());
    }

    // List branch: enumerate all.
    let rows = gather_rows(&layout, None)?;
    let rows = filter_rows(rows, args);
    let stdout = std::io::stdout();
    let mut h = stdout.lock();
    if args.json {
        emit_json(&mut h, &rows, false)?;
    } else {
        render_table(&mut h, &rows, ctx.no_color).map_err(io_to_cli)?;
    }
    Ok(())
}

fn io_to_cli(e: std::io::Error) -> CliError {
    CliError::Generic {
        reason: format!("write: {e}"),
    }
}

/// Emit JSON: array for list mode, single object for detail.
fn emit_json<W: Write>(out: &mut W, rows: &[Row], single: bool) -> Result<(), CliError> {
    let values: Vec<Value> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Live(_, s) => serde_json::to_value(s).ok(),
            Row::Archived(_, s) => {
                // Same AgentStatus shape as Live so downstream
                // consumers don't need a union type. Adorn with a
                // `source` marker so scripts can distinguish a
                // socket-fresh snapshot from a persisted one.
                let mut v = serde_json::to_value(s).ok()?;
                if let Value::Object(ref mut m) = v {
                    m.insert("source".to_string(), Value::String("status.json".into()));
                }
                Some(v)
            }
            Row::Orphan(id) => Some(json!({
                "id": id.as_str(),
                "phase": "orphan",
            })),
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

/// Dispatch `ark list` — T-088. `--watch` loops `run_once`
/// every 2s with an ANSI clear-screen between frames; Ctrl-C
/// terminates the loop (handled by the shell / OS).
pub fn run(args: ListArgs, ctx: &Ctx) -> Result<(), CliError> {
    if args.watch && !args.json {
        loop {
            // ANSI clear-screen + home cursor. Skipped when no_color
            // so piped output stays clean.
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
    use ark_types::{AgentSpec, AgentStatus, Findings, Phase};
    use clap::Parser;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use ulid::Ulid;

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
    fn status_filter_parses() {
        let h = Host::try_parse_from(["list", "--status", "running"]).expect("parse");
        assert_eq!(h.args.status.as_deref(), Some("running"));
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

    fn ulid_a() -> Ulid {
        Ulid::from_string("01JX7Z8K6X9Y2ZT4ABCDEF0123").expect("ulid a")
    }
    fn ulid_b() -> Ulid {
        Ulid::from_string("01JX7Z8K6X9Y2ZT4ABCDEF0456").expect("ulid b")
    }

    fn mk_status(id: &AgentId, phase: Phase, orch: &str) -> AgentStatus {
        let spec = AgentSpec::new(
            id.clone(),
            id.name(),
            orch,
            "claude-code",
            PathBuf::from("/tmp/w"),
            vec!["claude".into()],
        );
        AgentStatus {
            spec,
            phase,
            progress: None,
            last_event_at: chrono::Utc::now(),
            last_event_summary: "summary".into(),
            tab_handles: Vec::new(),
            supervisor_pid: 1,
            stalled_since: None,
            findings: Findings::default(),
            hide: false,
        }
    }

    fn seed_agent(layout: &StateLayout, id: &AgentId) {
        fs::create_dir_all(layout.agent_dir(id)).expect("mkdir");
    }

    // ---------- table rendering ----------

    #[test]
    fn table_renders_header_and_rows_no_color() {
        let id = AgentId::from_parts("cavekit", "auth", ulid_a());
        let rows = vec![Row::Live(
            id.clone(),
            mk_status(&id, Phase::Running, "cavekit"),
        )];
        let mut buf: Vec<u8> = Vec::new();
        render_table(&mut buf, &rows, true).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("ID"));
        assert!(s.contains("NAME"));
        assert!(s.contains("PHASE"));
        assert!(s.contains("running"));
        assert!(s.contains("cavekit"));
        // No ANSI when no_color=true.
        assert!(!s.contains("\x1b["), "unexpected ANSI: {s:?}");
    }

    #[test]
    fn table_uses_ansi_bold_when_color_allowed() {
        let rows: Vec<Row> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        render_table(&mut buf, &rows, false).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        // Bold ANSI escape must appear when color is allowed.
        assert!(s.contains("\x1b[1m"), "expected ANSI bold: {s:?}");
    }

    #[test]
    fn table_empty_shows_no_agents_marker() {
        let rows: Vec<Row> = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        render_table(&mut buf, &rows, true).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("(no agents)"));
    }

    // ---------- filtering ----------

    #[test]
    fn status_filter_keeps_only_matching_rows() {
        let a = AgentId::from_parts("cavekit", "one", ulid_a());
        let b = AgentId::from_parts("cavekit", "two", ulid_b());
        let rows = vec![
            Row::Live(a.clone(), mk_status(&a, Phase::Running, "cavekit")),
            Row::Live(b.clone(), mk_status(&b, Phase::Done, "cavekit")),
        ];
        let args = ListArgs {
            id: None,
            orchestrator: None,
            status: Some("running".into()),
            json: false,
            watch: false,
        };
        let filtered = filter_rows(rows, &args);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].phase_str(), "running");
    }

    #[test]
    fn orchestrator_filter_keeps_only_matching_rows() {
        let a = AgentId::from_parts("cavekit", "one", ulid_a());
        let b = AgentId::from_parts("claudecode", "two", ulid_b());
        let rows = vec![
            Row::Live(a.clone(), mk_status(&a, Phase::Running, "cavekit")),
            Row::Live(b.clone(), mk_status(&b, Phase::Running, "claudecode")),
        ];
        let args = ListArgs {
            id: None,
            orchestrator: Some("claudecode".into()),
            status: None,
            json: false,
            watch: false,
        };
        let filtered = filter_rows(rows, &args);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].orchestrator(), "claudecode");
    }

    // ---------- JSON shape ----------

    #[test]
    fn json_emits_array_for_list_mode() {
        let id = AgentId::from_parts("cavekit", "auth", ulid_a());
        let rows = vec![Row::Live(
            id.clone(),
            mk_status(&id, Phase::Idle, "cavekit"),
        )];
        let mut buf: Vec<u8> = Vec::new();
        emit_json(&mut buf, &rows, false).expect("emit");
        let s = String::from_utf8(buf).expect("utf8");
        let v: Value = serde_json::from_str(&s).expect("json");
        assert!(v.is_array(), "expected array, got {s}");
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["phase"], "idle");
    }

    #[test]
    fn json_single_mode_emits_object() {
        let id = AgentId::from_parts("cavekit", "auth", ulid_a());
        let rows = vec![Row::Live(
            id.clone(),
            mk_status(&id, Phase::Idle, "cavekit"),
        )];
        let mut buf: Vec<u8> = Vec::new();
        emit_json(&mut buf, &rows, true).expect("emit");
        let s = String::from_utf8(buf).expect("utf8");
        let v: Value = serde_json::from_str(&s).expect("json");
        assert!(v.is_object(), "expected object, got {s}");
        assert_eq!(v["phase"], "idle");
    }

    #[test]
    fn json_orphan_row_emits_minimal_shape() {
        let id = AgentId::from_parts("cavekit", "dead", ulid_a());
        let rows = vec![Row::Orphan(id.clone())];
        let mut buf: Vec<u8> = Vec::new();
        emit_json(&mut buf, &rows, false).expect("emit");
        let v: Value = serde_json::from_slice(&buf).expect("json");
        let arr = v.as_array().expect("array");
        assert_eq!(arr[0]["phase"], "orphan");
        assert_eq!(arr[0]["id"], id.as_str());
    }

    // ---------- phase parsing ----------

    #[test]
    fn unknown_status_yields_generic_err() {
        let tmp = tempdir().expect("tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let args = ListArgs {
            id: None,
            orchestrator: None,
            status: Some("nonsense".into()),
            json: false,
            watch: false,
        };
        let err = run_once(&args, &ctx).expect_err("should reject");
        assert!(matches!(err, CliError::Generic { .. }));
    }

    #[test]
    fn known_phase_passes_validation() {
        assert!(is_known_phase("running"));
        assert!(is_known_phase("killed"));
        assert!(is_known_phase("timeout"));
        assert!(!is_known_phase("bogus"));
    }

    // ---------- resolver error mapping ----------

    #[test]
    fn ambiguous_id_returns_ambiguous_err() {
        let tmp = tempdir().expect("tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let a = AgentId::from_parts("cavekit", "auth", ulid_a());
        let b = AgentId::from_parts("cavekit", "auth", ulid_b());
        seed_agent(&layout, &a);
        seed_agent(&layout, &b);

        let args = ListArgs {
            id: Some("cavekit-auth".into()),
            orchestrator: None,
            status: None,
            json: false,
            watch: false,
        };
        let err = run_once(&args, &ctx).expect_err("ambiguous");
        match err {
            CliError::Ambiguous { candidates, .. } => assert_eq!(candidates.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn not_found_id_returns_not_found_err() {
        let tmp = tempdir().expect("tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let args = ListArgs {
            id: Some("ghost".into()),
            orchestrator: None,
            status: None,
            json: false,
            watch: false,
        };
        let err = run_once(&args, &ctx).expect_err("should be not-found");
        assert!(matches!(err, CliError::NotFound { .. }));
    }

    // ---------- missing socket → orphan row ----------

    #[test]
    fn missing_socket_yields_orphan_row_in_table_mode() {
        // Agent dir exists; no socket. gather_rows must surface
        // it as Row::Orphan rather than erroring.
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
        let id = AgentId::from_parts("cavekit", "dead", ulid_a());
        seed_agent(&layout, &id);

        let rows = gather_rows(&layout, None).expect("gather");
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], Row::Orphan(_)));
        assert_eq!(rows[0].phase_str(), "orphan");

        // And it renders cleanly.
        let mut buf: Vec<u8> = Vec::new();
        render_table(&mut buf, &rows, true).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("orphan"));
        assert!(s.contains("dead"));
    }

    // ---------- detail view ----------

    #[test]
    fn detail_view_prints_spec_fields() {
        let id = AgentId::from_parts("cavekit", "auth", ulid_a());
        let row = Row::Live(id.clone(), mk_status(&id, Phase::Running, "cavekit"));
        let mut buf: Vec<u8> = Vec::new();
        render_detail(&mut buf, &row, true).expect("detail");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("id:"));
        assert!(s.contains("cwd:"));
        assert!(s.contains("engine:"));
        assert!(s.contains("claude-code"));
    }

    // ---------- watch flag semantics ----------

    #[test]
    fn watch_with_json_falls_through_to_run_once() {
        // --watch + --json is "TTY mode" — we must NOT enter the
        // infinite loop. Exercise the branch by running `run`
        // with an empty state dir and verifying it returns Ok.
        let tmp = tempdir().expect("tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let args = ListArgs {
            id: None,
            orchestrator: None,
            status: None,
            json: true,
            watch: true,
        };
        // Should return (not loop), because --json forces single pass.
        run(args, &ctx).expect("should not loop under --json");
    }

    #[test]
    fn short_id_truncates_long_ids() {
        let id = AgentId::from_parts("cavekit", "foo", ulid_a());
        // full id is longer than 12 chars
        assert_eq!(short_id(&id).chars().count(), 12);
    }

    #[test]
    fn format_uptime_compact_under_minute() {
        let now = chrono::Utc::now();
        assert_eq!(format_uptime_since(now), "0s");
    }

    // ---------- F-518: status.json fallback → Row::Archived -----------

    fn write_status_json(layout: &StateLayout, id: &AgentId, status: &AgentStatus) {
        let dir = layout.agent_dir(id);
        fs::create_dir_all(&dir).expect("mkdir agent dir");
        let body = serde_json::to_vec_pretty(status).expect("serialize");
        fs::write(layout.status_path(id), body).expect("write status.json");
    }

    #[test]
    fn missing_socket_with_status_json_yields_archived_row() {
        // No socket → read_persisted_status() picks up status.json →
        // gather_rows emits Row::Archived with the persisted phase.
        let tmp = tempfile::Builder::new()
            .prefix("arklist-arch")
            .tempdir_in("/tmp")
            .expect("short tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = AgentId::from_parts("cavekit", "archived", ulid_a());
        let archived = mk_status(&id, Phase::Done, "cavekit");
        write_status_json(&layout, &id, &archived);

        let rows = gather_rows(&layout, None).expect("gather");
        assert_eq!(rows.len(), 1);
        match &rows[0] {
            Row::Archived(got_id, got_status) => {
                assert_eq!(got_id, &id);
                assert_eq!(got_status.phase, Phase::Done);
                assert_eq!(got_status.spec.name, "archived");
            }
            other => panic!("expected Archived, got {other:?}"),
        }
        assert_eq!(rows[0].phase_str(), "done");
    }

    #[test]
    fn missing_socket_without_status_json_yields_orphan_row() {
        // Regression guard for the original Orphan path: when neither
        // socket nor status.json exist, the row is still Orphan.
        let tmp = tempfile::Builder::new()
            .prefix("arklist-orph")
            .tempdir_in("/tmp")
            .expect("short tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = AgentId::from_parts("cavekit", "genuine-orphan", ulid_a());
        seed_agent(&layout, &id); // dir only, no socket, no status.json

        let rows = gather_rows(&layout, None).expect("gather");
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], Row::Orphan(_)));
    }

    #[test]
    fn read_persisted_status_returns_none_when_missing() {
        let tmp = tempfile::Builder::new()
            .prefix("arklist-read")
            .tempdir_in("/tmp")
            .expect("short tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        let id = AgentId::from_parts("cavekit", "nope", ulid_a());
        assert!(read_persisted_status(&layout, &id).is_none());
    }

    #[test]
    fn archived_row_renders_persisted_phase_in_table() {
        // Row::Archived must show its persisted phase in the table
        // (not "orphan") so users can see lifecycle outcomes after
        // supervisors have exited.
        let id = AgentId::from_parts("cavekit", "old", ulid_a());
        let rows = vec![Row::Archived(
            id.clone(),
            mk_status(&id, Phase::Killed, "cavekit"),
        )];
        let mut buf: Vec<u8> = Vec::new();
        render_table(&mut buf, &rows, true).expect("render");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("killed"), "expected phase killed in table: {s}");
        assert!(
            !s.contains("orphan"),
            "archived rows must not show 'orphan'"
        );
    }

    #[test]
    fn archived_row_json_has_source_marker() {
        // JSON emission must let callers distinguish fresh-socket
        // snapshots from archived persisted ones.
        let id = AgentId::from_parts("cavekit", "done", ulid_a());
        let rows = vec![Row::Archived(
            id.clone(),
            mk_status(&id, Phase::Done, "cavekit"),
        )];
        let mut buf: Vec<u8> = Vec::new();
        emit_json(&mut buf, &rows, false).expect("emit");
        let v: Value = serde_json::from_slice(&buf).expect("json");
        let arr = v.as_array().expect("array");
        assert_eq!(arr[0]["phase"], "done");
        assert_eq!(arr[0]["source"], "status.json");
    }

    // ---------- F-505: unreadable agents_root surfaces as CliError::Generic ----------

    #[cfg(unix)]
    #[test]
    fn gather_rows_surfaces_io_failure_when_agents_root_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::Builder::new()
            .prefix("arklist-perm")
            .tempdir_in("/tmp")
            .expect("tempdir");
        let ctx = layout_ctx(tmp.path().to_path_buf());
        let layout = StateLayout::new(
            ctx.state_dir.clone(),
            ctx.runtime_dir.clone(),
            ctx.config_dir.clone(),
        );
        // Create agents_root first, then chmod 000 to force EACCES on
        // read_dir. Missing dir is handled separately (returns empty).
        let agents_root = layout.agents_root();
        fs::create_dir_all(&agents_root).expect("mkdir agents_root");
        let mut perms = fs::metadata(&agents_root).expect("meta").permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&agents_root, perms).expect("chmod 000");

        let result = gather_rows(&layout, None);

        // Restore perms so tempdir can be cleaned up.
        let mut restore = fs::metadata(&agents_root).expect("meta").permissions();
        restore.set_mode(0o755);
        fs::set_permissions(&agents_root, restore).ok();

        // root+writable test environments may bypass mode 000; skip the
        // assertion gracefully there rather than produce a false failure.
        if nix::unistd::Uid::effective().is_root() {
            return;
        }

        match result {
            Err(CliError::Generic { reason }) => {
                assert!(
                    reason.contains("agents_root"),
                    "reason should mention agents_root: {reason}"
                );
            }
            Err(other) => panic!("expected Generic, got {other:?}"),
            Ok(rows) => panic!("expected Err, got Ok({rows:?})"),
        }
    }
}
