//! `ark config` — show/edit/get/set (cavekit-cli R6, T-090).
//!
//! # Layering
//! `show` and `get` resolve the *effective* config by layering:
//!   defaults -> user TOML -> project TOML -> `ARK_*` env.
//! `set` writes only to the user file at
//! `{ctx.config_dir}/config.toml`, consistent with `ark config edit`.
//!
//! # Comment preservation caveat
//! `set` round-trips through `toml::Value`, which drops comments
//! and whitespace. Users who want commented templates should edit
//! the file directly via `ark config edit`. Structural keys and
//! values ARE preserved; only trailing / inline comments are lost.

use std::path::{Path, PathBuf};
use std::process::Command;

use ark_config::{ConfigLoader, DEFAULT_ENV_PREFIX, TEMPLATE_CONFIG_TOML, schema::Config};
use clap::{Args, Subcommand};

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark config`.
#[derive(Debug, Args)]
#[command(
    about = "Show / edit / get / set configuration values",
    long_about = "Inspect or modify the effective ark configuration.\n\
                  Values are written to\n\
                  $XDG_CONFIG_HOME/ark/config.toml.\n\
                  \n\
                  Examples:\n  \
                  ark config show\n  \
                  ark config get orchestrator.cavekit.default_layout\n  \
                  ark config set \\\n    \
                    orchestrator.cavekit.default_layout triple-stack\n  \
                  ark config edit"
)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

/// The four config verbs (R6).
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the effective config (after figment layering) as TOML.
    ///
    /// Example:
    ///   ark config show
    Show,

    /// Open $EDITOR on the user config file.
    ///
    /// Example:
    ///   ark config edit
    Edit,

    /// Print a single value by dot-path.
    ///
    /// Example:
    ///   ark config get orchestrator.cavekit.default_layout
    Get {
        /// Dot-path key (e.g. `orchestrator.cavekit.default_layout`).
        #[arg(value_name = "KEY")]
        key: String,
    },

    /// Set a single value by dot-path. Validates before writing.
    ///
    /// Example:
    ///   ark config set orchestrator.cavekit.default_layout triple-stack
    Set {
        /// Dot-path key.
        #[arg(value_name = "KEY")]
        key: String,
        /// Value (TOML-compatible literal).
        #[arg(value_name = "VAL")]
        val: String,
    },
}

/// Env var that overrides the user config file location.
/// ark-config crate already documents this env var as the single-file
/// override (see `crates/config/src/lib.rs` RESERVED_ENV_VARS).
const ARK_CONFIG_PATH_ENV: &str = "ARK_CONFIG_PATH";

/// Path of the user config file for a given ctx.
///
/// Precedence: `$ARK_CONFIG_PATH` (when set and non-empty) wins over
/// the default `{ctx.config_dir}/config.toml`. This lets a caller
/// point all four config subcommands at an alternate TOML file
/// without mutating ctx / the directory layout.
fn user_config_path(ctx: &Ctx) -> PathBuf {
    if let Some(p) = std::env::var_os(ARK_CONFIG_PATH_ENV)
        && !p.is_empty()
    {
        return PathBuf::from(p);
    }
    ctx.config_dir.join("config.toml")
}

/// Path of the project config file: `./.ark/config.toml`.
fn project_config_path() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .map(|d| d.join(".ark").join("config.toml"))
}

/// Build a loader that mirrors `show`/`get` resolution semantics.
fn effective_loader(ctx: &Ctx) -> ConfigLoader {
    ConfigLoader::new()
        .with_user_path(Some(user_config_path(ctx)))
        .with_project_path(project_config_path())
        .with_env_prefix(DEFAULT_ENV_PREFIX)
}

/// Load the effective `Config`, translating any figment error to
/// [`CliError::ConfigError`].
fn load_effective(ctx: &Ctx) -> Result<Config, CliError> {
    effective_loader(ctx)
        .load::<Config>()
        .map_err(|e| CliError::ConfigError {
            reason: e.to_string(),
        })
}

/// Traverse a `toml::Value` with a dotted path.
fn walk_dotted<'a>(root: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    let mut cur = root;
    for segment in key.split('.') {
        let table = cur.as_table()?;
        cur = table.get(segment)?;
    }
    Some(cur)
}

/// Parse a user-supplied string as a TOML value.  Falls back to
/// a bare string when the input isn't valid TOML syntax, so that
/// `ark config set foo.bar hello` Just Works.
fn parse_value(raw: &str) -> toml::Value {
    // Wrap in a key=value line so scalars parse cleanly.
    let wrapped = format!("__v = {raw}");
    if let Ok(parsed) = wrapped.parse::<toml::Value>()
        && let Some(v) = parsed.get("__v")
    {
        return v.clone();
    }
    toml::Value::String(raw.to_string())
}

/// Insert `value` at `key` (dotted path) inside `root`, creating
/// intermediate tables as needed.
fn insert_dotted(
    root: &mut toml::value::Table,
    key: &str,
    value: toml::Value,
) -> Result<(), CliError> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return Err(CliError::ConfigError {
            reason: format!("invalid dotted key: {key:?}"),
        });
    }
    let mut cur = root;
    for seg in &parts[..parts.len() - 1] {
        let entry = cur
            .entry((*seg).to_string())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        match entry {
            toml::Value::Table(t) => cur = t,
            _ => {
                return Err(CliError::ConfigError {
                    reason: format!("segment {seg:?} is not a table in key {key:?}"),
                });
            }
        }
    }
    cur.insert(parts[parts.len() - 1].to_string(), value);
    Ok(())
}

/// Ensure the parent directory of `path` exists.
fn ensure_parent(path: &Path) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CliError::ConfigError {
            reason: format!("create {}: {e}", parent.display()),
        })?;
    }
    Ok(())
}

/// Read the user config file as a TOML table (empty if missing).
fn read_user_table(path: &Path) -> Result<toml::value::Table, CliError> {
    if !path.exists() {
        return Ok(toml::value::Table::new());
    }
    let s = std::fs::read_to_string(path).map_err(|e| CliError::ConfigError {
        reason: format!("read {}: {e}", path.display()),
    })?;
    s.parse::<toml::Value>()
        .map_err(|e| CliError::ConfigError {
            reason: format!("parse {}: {e}", path.display()),
        })?
        .as_table()
        .cloned()
        .ok_or_else(|| CliError::ConfigError {
            reason: format!("{} is not a TOML table", path.display()),
        })
}

/// `ark config show` — print the effective config as pretty TOML.
fn run_show(ctx: &Ctx) -> Result<(), CliError> {
    let cfg = load_effective(ctx)?;
    let out = toml::to_string_pretty(&cfg).map_err(|e| CliError::ConfigError {
        reason: format!("serialize effective config: {e}"),
    })?;
    println!("{out}");
    Ok(())
}

/// F-515: decide the trailing argv (after the parsed EDITOR words) for
/// the editor spawn. For a `sh -c "<script>"` / `bash -c "<script>"`
/// wrapper the shell consumes subsequent argv entries as `$0, $1, $2,
/// …`; if we merely append the config path it becomes `$0` (the
/// wrapper's "script name") instead of `$1`, so the user's `"$1"`
/// reference in the inner command expands to empty and the editor
/// opens nothing. When we recognize that pattern (exactly three tokens:
/// `sh|bash`, `-c`, `<script>`) we prepend a dummy `"ark-edit"` so the
/// path lands at `$1`. Every other EDITOR shape just appends the path.
fn build_editor_argv_tail(parts: &[String], path: &std::path::Path) -> Vec<std::ffi::OsString> {
    let is_sh_c_wrapper =
        parts.len() == 3 && (parts[0] == "sh" || parts[0] == "bash") && parts[1] == "-c";
    if is_sh_c_wrapper {
        vec![
            std::ffi::OsString::from("ark-edit"),
            path.as_os_str().to_os_string(),
        ]
    } else {
        vec![path.as_os_str().to_os_string()]
    }
}

/// `ark config edit` — spawn `$EDITOR` on the user config file.
/// Creates the file (with the shipped template) if absent.
fn run_edit(ctx: &Ctx) -> Result<(), CliError> {
    let path = user_config_path(ctx);
    if !path.exists() {
        ensure_parent(&path)?;
        std::fs::write(&path, TEMPLATE_CONFIG_TOML).map_err(|e| CliError::ConfigError {
            reason: format!("write template to {}: {e}", path.display()),
        })?;
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    // Split the editor string via POSIX shell rules so quoted paths
    // (`"/Applications/Sublime Text.app/.../subl" --wait`) and
    // sh-c wrappers (`sh -c "vim \$1"`) parse correctly (F-510).
    let parts = shlex::split(&editor).ok_or_else(|| CliError::ConfigError {
        reason: format!("invalid $EDITOR syntax: {editor:?}"),
    })?;
    if parts.is_empty() {
        return Err(CliError::ConfigError {
            reason: "$EDITOR is empty".into(),
        });
    }
    // F-515: when EDITOR is a `sh -c "…"` / `bash -c "…"` wrapper, the
    // shell interprets subsequent argv entries as `$0, $1, $2, …` of
    // the inner script. Simply appending the config path would make
    // it `$0` (the shell's "script name"), not `$1`, so the wrapper
    // opens nothing. Insert a dummy `$0` before the path so the path
    // lands at `$1` where users expect.
    let argv_tail = build_editor_argv_tail(&parts, &path);
    let status = Command::new(&parts[0])
        .args(&parts[1..])
        .args(&argv_tail)
        .status()
        .map_err(|e| CliError::Generic {
            reason: format!("spawn editor {editor:?}: {e}"),
        })?;
    if status.success() {
        Ok(())
    } else {
        let code = status.code().unwrap_or(1);
        Err(CliError::Generic {
            reason: format!("editor exited with code {code}"),
        })
    }
}

/// `ark config get <KEY>` — print leaf value for a dotted path.
fn run_get(ctx: &Ctx, key: &str) -> Result<(), CliError> {
    let cfg = load_effective(ctx)?;
    let as_toml = toml::Value::try_from(&cfg).map_err(|e| CliError::ConfigError {
        reason: format!("serialize for lookup: {e}"),
    })?;
    let leaf = walk_dotted(&as_toml, key).ok_or_else(|| CliError::NotFound {
        what: format!("config key {key:?}"),
    })?;
    match leaf {
        toml::Value::String(s) => println!("{s}"),
        other => println!("{other}"),
    }
    Ok(())
}

/// `ark config set <KEY> <VAL>` — write to user config file.
/// Comments in the file are NOT preserved (see module doc).
///
/// Validates the edited table against the [`Config`] schema BEFORE
/// touching the real file (F-508). Validation is done by serializing
/// the updated table to a sibling temp file, running the full layered
/// `ConfigLoader::load::<Config>()` against it, and only renaming the
/// temp over the real file if deserialization succeeds. On schema
/// failure the original file is left untouched.
fn run_set(ctx: &Ctx, key: &str, val: &str) -> Result<(), CliError> {
    let path = user_config_path(ctx);
    ensure_parent(&path)?;
    let mut table = read_user_table(&path)?;
    let parsed = parse_value(val);
    insert_dotted(&mut table, key, parsed)?;
    let rendered =
        toml::to_string_pretty(&toml::Value::Table(table)).map_err(|e| CliError::ConfigError {
            reason: format!("serialize user table: {e}"),
        })?;

    // Write to a sibling temp file so a validation failure leaves the
    // real config.toml on disk exactly as it was.
    let parent = path.parent().ok_or_else(|| CliError::ConfigError {
        reason: format!("{} has no parent directory", path.display()),
    })?;
    let tmp_path = parent.join(format!(".config.toml.tmp.{}", std::process::id()));
    std::fs::write(&tmp_path, &rendered).map_err(|e| CliError::ConfigError {
        reason: format!("write temp {}: {e}", tmp_path.display()),
    })?;

    // Validate: load the edited TOML through the same layered loader
    // used by `show`/`get`, but with the temp file standing in for the
    // user layer. Project and env layers are intentionally skipped so
    // a valid write isn't rejected by an unrelated env override.
    let validate = ConfigLoader::new()
        .with_user_path(Some(tmp_path.clone()))
        .load::<Config>();
    if let Err(e) = validate {
        // Remove the temp file; ignore errors (best-effort cleanup).
        let _ = std::fs::remove_file(&tmp_path);
        return Err(CliError::ConfigError {
            reason: format!("invalid value for {key:?}: {e}"),
        });
    }

    // Atomic rename over the real file.
    std::fs::rename(&tmp_path, &path).map_err(|e| CliError::ConfigError {
        reason: format!("rename {} -> {}: {e}", tmp_path.display(), path.display()),
    })?;
    Ok(())
}

/// Dispatch entry-point for `ark config ...`.
pub fn run(args: ConfigArgs, ctx: &Ctx) -> Result<(), CliError> {
    match args.command {
        ConfigCommand::Show => run_show(ctx),
        ConfigCommand::Edit => run_edit(ctx),
        ConfigCommand::Get { key } => run_get(ctx, &key),
        ConfigCommand::Set { key, val } => run_set(ctx, &key, &val),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::ENV_LOCK;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Host {
        #[command(flatten)]
        args: ConfigArgs,
    }

    #[test]
    fn show_subcommand_parses() {
        let h = Host::try_parse_from(["config", "show"]).expect("parse");
        assert!(matches!(h.args.command, ConfigCommand::Show));
    }

    #[test]
    fn edit_subcommand_parses() {
        let h = Host::try_parse_from(["config", "edit"]).expect("parse");
        assert!(matches!(h.args.command, ConfigCommand::Edit));
    }

    #[test]
    fn get_subcommand_requires_key() {
        let err = Host::try_parse_from(["config", "get"]).expect_err("need key");
        assert!(err.to_string().contains("KEY") || err.to_string().contains("required"));
    }

    #[test]
    fn get_subcommand_parses_key() {
        let h = Host::try_parse_from(["config", "get", "a.b.c"]).expect("parse");
        match h.args.command {
            ConfigCommand::Get { key } => assert_eq!(key, "a.b.c"),
            other => panic!("expected Get, got {other:?}"),
        }
    }

    #[test]
    fn set_subcommand_parses_key_and_val() {
        let h = Host::try_parse_from(["config", "set", "a.b", "42"]).expect("parse");
        match h.args.command {
            ConfigCommand::Set { key, val } => {
                assert_eq!(key, "a.b");
                assert_eq!(val, "42");
            }
            other => panic!("expected Set, got {other:?}"),
        }
    }

    #[test]
    fn set_subcommand_requires_val() {
        let err = Host::try_parse_from(["config", "set", "a.b"]).expect_err("need val");
        assert!(err.to_string().contains("VAL") || err.to_string().contains("required"));
    }

    #[test]
    fn missing_subcommand_errors() {
        let err = Host::try_parse_from(["config"]).expect_err("need subcommand");
        assert!(
            err.to_string().contains("subcommand")
                || err.to_string().contains("show")
                || err.to_string().contains("required")
        );
    }

    // ------------------------------------------------------------------
    // Handler tests.  These mutate process env (HOME / ARK_CONFIG_DIR /
    // EDITOR) so hold ENV_LOCK.
    // ------------------------------------------------------------------

    fn ctx_for(config_dir: &Path) -> Ctx {
        Ctx {
            no_color: true,
            log_level: "info".into(),
            state_dir: config_dir.to_path_buf(),
            config_dir: config_dir.to_path_buf(),
            runtime_dir: config_dir.to_path_buf(),
        }
    }

    #[test]
    fn show_renders_user_config_as_toml() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[defaults]\norchestrator = \"cavekit\"\n").unwrap();
        let ctx = ctx_for(tmp.path());
        // Ensure no cwd .ark overrides and no env wins over file.
        let cfg = load_effective(&ctx).expect("load");
        assert_eq!(cfg.defaults.orchestrator, "cavekit");

        // Also exercise the rendering path.
        let rendered = toml::to_string_pretty(&cfg).unwrap();
        assert!(rendered.contains("orchestrator = \"cavekit\""));
    }

    #[test]
    fn get_returns_leaf_via_dotted_path() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "[orchestrator.cavekit]\ndefault_layout = \"triple-stack\"\n",
        )
        .unwrap();
        let ctx = ctx_for(tmp.path());
        let cfg = load_effective(&ctx).expect("load");
        let v = toml::Value::try_from(&cfg).unwrap();
        let leaf = walk_dotted(&v, "orchestrator.cavekit.default_layout").expect("leaf");
        assert_eq!(leaf.as_str(), Some("triple-stack"));
    }

    #[test]
    fn get_missing_key_is_not_found() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_for(tmp.path());
        let err = run_get(&ctx, "nope.definitely.not.here").expect_err("missing key should error");
        assert!(matches!(err, CliError::NotFound { .. }));
    }

    #[test]
    fn set_round_trips_value_into_user_file() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_for(tmp.path());
        run_set(&ctx, "defaults.orchestrator", "\"cavekit\"").expect("set");

        let path = user_config_path(&ctx);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("orchestrator = \"cavekit\""));

        // Re-loading through the effective loader must see the new value.
        let cfg = load_effective(&ctx).expect("reload");
        assert_eq!(cfg.defaults.orchestrator, "cavekit");
    }

    #[test]
    fn set_creates_nested_tables_and_parses_scalars() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_for(tmp.path());
        // Raw scalar (no quotes) still parses via the `__v = <raw>`
        // wrapper.
        run_set(&ctx, "diff.debounce_ms", "500").expect("set int");
        let cfg = load_effective(&ctx).expect("reload");
        assert_eq!(cfg.diff.debounce_ms, 500);
    }

    #[test]
    fn edit_creates_template_when_missing() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_for(tmp.path());
        // No-op editor: `true` exits 0 without touching the file.
        let prior = std::env::var("EDITOR").ok();
        // Safety: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var("EDITOR", "true");
        }
        let result = run_edit(&ctx);
        unsafe {
            match prior {
                Some(v) => std::env::set_var("EDITOR", v),
                None => std::env::remove_var("EDITOR"),
            }
        }
        result.expect("edit ok");

        let path = user_config_path(&ctx);
        assert!(path.exists(), "template must be created when missing");
        let contents = std::fs::read_to_string(&path).unwrap();
        // Sanity: the shipped template has the R5 env-shortcut doc.
        assert!(contents.contains("ARK_ORCHESTRATOR"));
    }

    #[test]
    fn parse_value_handles_string_and_int_and_bool() {
        assert_eq!(parse_value("42").as_integer(), Some(42));
        assert_eq!(parse_value("true").as_bool(), Some(true));
        assert_eq!(parse_value("\"hi\"").as_str(), Some("hi"));
        // Bare unquoted word -> fallback to string.
        assert_eq!(parse_value("bareword").as_str(), Some("bareword"));
    }

    #[test]
    fn insert_dotted_rejects_empty_segment() {
        let mut t = toml::value::Table::new();
        let err = insert_dotted(&mut t, "a..b", toml::Value::Integer(1))
            .expect_err("empty segment rejected");
        assert!(matches!(err, CliError::ConfigError { .. }));
    }

    #[test]
    fn user_config_path_honors_ark_config_path_env() {
        // F-502: when ARK_CONFIG_PATH is set, it overrides the
        // default {ctx.config_dir}/config.toml for all four
        // subcommands.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_for(tmp.path());
        let override_path = tmp.path().join("custom").join("override.toml");

        let prior = std::env::var_os(ARK_CONFIG_PATH_ENV);
        // Safety: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var(ARK_CONFIG_PATH_ENV, &override_path);
        }
        let got = user_config_path(&ctx);
        unsafe {
            match prior {
                Some(v) => std::env::set_var(ARK_CONFIG_PATH_ENV, v),
                None => std::env::remove_var(ARK_CONFIG_PATH_ENV),
            }
        }
        assert_eq!(got, override_path);
    }

    #[test]
    fn user_config_path_falls_back_to_ctx_config_dir() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_for(tmp.path());

        let prior = std::env::var_os(ARK_CONFIG_PATH_ENV);
        // Safety: guarded by ENV_LOCK.
        unsafe {
            std::env::remove_var(ARK_CONFIG_PATH_ENV);
        }
        let got = user_config_path(&ctx);
        unsafe {
            if let Some(v) = prior {
                std::env::set_var(ARK_CONFIG_PATH_ENV, v);
            }
        }
        assert_eq!(got, ctx.config_dir.join("config.toml"));
    }

    #[test]
    fn set_writes_to_ark_config_path_override() {
        // Higher-level regression for F-502: run_set must write to
        // the path indicated by ARK_CONFIG_PATH, not the ctx dir.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        // Ctx dir is distinct from the override target so a stray
        // write to config_dir/config.toml wouldn't satisfy the assert.
        let ctx_dir = tmp.path().join("ctxdir");
        std::fs::create_dir_all(&ctx_dir).unwrap();
        let ctx = ctx_for(&ctx_dir);
        let override_path = tmp.path().join("alt").join("override.toml");

        let prior = std::env::var_os(ARK_CONFIG_PATH_ENV);
        // Safety: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var(ARK_CONFIG_PATH_ENV, &override_path);
        }
        let result = run_set(&ctx, "defaults.orchestrator", "\"cavekit\"");
        unsafe {
            match prior {
                Some(v) => std::env::set_var(ARK_CONFIG_PATH_ENV, v),
                None => std::env::remove_var(ARK_CONFIG_PATH_ENV),
            }
        }
        result.expect("set ok");
        assert!(
            override_path.exists(),
            "set must write to ARK_CONFIG_PATH override"
        );
        assert!(
            !ctx_dir.join("config.toml").exists(),
            "set must NOT fall back to ctx.config_dir when env is set"
        );
    }

    #[test]
    fn run_dispatches_show_without_panic() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_for(tmp.path());
        let args = Host::try_parse_from(["config", "show"]).unwrap().args;
        run(args, &ctx).expect("show runs");
    }

    // ------------------------------------------------------------------
    // F-508: `set` must validate before persisting.
    // ------------------------------------------------------------------

    #[test]
    fn set_rejects_schema_invalid_value_and_preserves_original_file() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_for(tmp.path());
        let path = user_config_path(&ctx);

        // Seed a known-good original file.
        ensure_parent(&path).unwrap();
        let original = "[diff]\ndebounce_ms = 250\n";
        std::fs::write(&path, original).unwrap();

        // Attempt to set `diff.debounce_ms` to a non-integer — the
        // schema wants a u64, so figment must reject on load.
        let err = run_set(&ctx, "diff.debounce_ms", "\"nope\"")
            .expect_err("schema-invalid value must be rejected");
        assert!(
            matches!(err, CliError::ConfigError { .. }),
            "expected ConfigError, got {err:?}"
        );

        // Original file must be untouched on disk.
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after, original,
            "config.toml must NOT be overwritten when validation fails"
        );

        // No temp file should be left behind.
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".config.toml.tmp.")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp file must be cleaned up on validation failure, found {leftovers:?}"
        );
    }

    #[test]
    fn set_accepts_schema_valid_value_via_validation_path() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_for(tmp.path());
        // Valid integer for diff.debounce_ms must succeed and load
        // back through the effective loader.
        run_set(&ctx, "diff.debounce_ms", "750").expect("valid set");
        let cfg = load_effective(&ctx).expect("reload");
        assert_eq!(cfg.diff.debounce_ms, 750);
    }

    // ------------------------------------------------------------------
    // F-510: `$EDITOR` parsing must handle quoted args / sh-c wrappers.
    // ------------------------------------------------------------------

    #[test]
    fn editor_parses_quoted_path_with_spaces() {
        // A quoted editor path with spaces plus a trailing flag must
        // yield two argv entries: the full path, and `--wait`.
        let editor = "\"/Applications/Sublime Text.app/Contents/SharedSupport/bin/subl\" --wait";
        let parts = shlex::split(editor).expect("parse ok");
        assert_eq!(
            parts,
            vec![
                "/Applications/Sublime Text.app/Contents/SharedSupport/bin/subl".to_string(),
                "--wait".to_string(),
            ]
        );
    }

    #[test]
    fn editor_parses_sh_c_wrapper_with_nested_quotes() {
        // `sh -c "vim $1"` is a common wrapper; shlex must produce
        // three tokens without mangling the inner quoted command.
        let editor = "sh -c \"vim $1\"";
        let parts = shlex::split(editor).expect("parse ok");
        assert_eq!(
            parts,
            vec!["sh".to_string(), "-c".to_string(), "vim $1".to_string()]
        );
    }

    #[test]
    fn editor_argv_tail_plain_editor_just_appends_path() {
        // F-515: non-wrapper shapes must behave exactly as before —
        // append the path as the last argv entry.
        let parts: Vec<String> = vec!["vim".into(), "--nofork".into()];
        let tail = build_editor_argv_tail(&parts, std::path::Path::new("/tmp/a.toml"));
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0], std::ffi::OsString::from("/tmp/a.toml"));
    }

    #[test]
    fn editor_argv_tail_sh_c_wrapper_inserts_dummy_zero_then_path() {
        // F-515: `sh -c "vim \"$1\""` needs an extra arg at position
        // $0 so the real file path lands at $1 where the inner script
        // expects it. Without this the editor opens nothing.
        let parts: Vec<String> = vec!["sh".into(), "-c".into(), "vim \"$1\"".into()];
        let tail = build_editor_argv_tail(&parts, std::path::Path::new("/tmp/a.toml"));
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0], std::ffi::OsString::from("ark-edit"));
        assert_eq!(tail[1], std::ffi::OsString::from("/tmp/a.toml"));
    }

    #[test]
    fn editor_argv_tail_bash_c_wrapper_also_inserts_dummy() {
        // F-515: `bash -c` behaves identically to `sh -c` w.r.t. $0/$1
        // positional expansion, so it gets the same treatment.
        let parts: Vec<String> = vec!["bash".into(), "-c".into(), "nvim \"$1\"".into()];
        let tail = build_editor_argv_tail(&parts, std::path::Path::new("/tmp/x.toml"));
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0], std::ffi::OsString::from("ark-edit"));
        assert_eq!(tail[1], std::ffi::OsString::from("/tmp/x.toml"));
    }

    #[test]
    fn editor_argv_tail_sh_without_dash_c_is_not_wrapper() {
        // F-515: guard against false positives — a longer argv that
        // happens to start with `sh` must NOT get the dummy $0 treatment.
        let parts: Vec<String> = vec!["sh".into(), "-x".into(), "-c".into(), "vim \"$1\"".into()];
        let tail = build_editor_argv_tail(&parts, std::path::Path::new("/tmp/a.toml"));
        // Only three-token `sh -c <script>` qualifies; this 4-token
        // form falls through to the plain append branch.
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0], std::ffi::OsString::from("/tmp/a.toml"));
    }

    #[test]
    fn editor_invalid_syntax_returns_config_error() {
        // Unclosed quote must surface as a ConfigError, not a panic
        // and not silently fall back to something weird.
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_for(tmp.path());
        let prior = std::env::var("EDITOR").ok();
        // Safety: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var("EDITOR", "\"unterminated");
        }
        let got = run_edit(&ctx);
        unsafe {
            match prior {
                Some(v) => std::env::set_var("EDITOR", v),
                None => std::env::remove_var("EDITOR"),
            }
        }
        match got {
            Err(CliError::ConfigError { reason }) => {
                assert!(
                    reason.contains("invalid $EDITOR syntax"),
                    "expected syntax error, got {reason:?}"
                );
            }
            other => panic!("expected ConfigError, got {other:?}"),
        }
    }
}
