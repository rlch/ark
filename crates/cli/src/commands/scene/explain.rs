//! `ark scene explain <ref>` — trace resolution of a specific ref.
//!
//! T-12.6 (cavekit-scene R13). Refs: `intent:<name>`,
//! `keybind:<chord>`, `plugin:<name>`, `reaction:<event-selector>`,
//! `ext:<name>`. Prints "defined at <file:line>; overridden by
//! <file:line>; final resolution: <origin>".
//!
//! Where `scene graph` (T-12.5) renders the entire attribution tree and
//! `scene explain-merge` (T-12.11) walks R11 composition across every
//! category, this command answers the narrow "where did THIS one thing
//! come from?" question. Given a single ref, it:
//!
//! 1. Loads the scene composition (same pipeline as `graph`).
//! 2. Finds every fragment whose AST contains a declaration matching
//!    the ref.
//! 3. Re-reads each contributing fragment's raw KDL to recover a
//!    `file:line` span for the declaration.
//! 4. Applies the R11 merge rule for the ref's category to pick the
//!    winner ("final resolution").
//!
//! The `ext:<name>` form is broader: it lists every contribution that
//! extension brought into the scene (use-activations, plus any `ext:`-
//! sourced plugins and intents documented by the extension's manifest).
//! v1 surfaces the simplest signal — `use "<name>"` activations, plus
//! any `plugin`s whose `source "ext:<name>"` points at this extension.

use std::path::{Path, PathBuf};

use clap::Args;
use kdl::KdlDocument;

use ark_scene::extends::SceneSearchCtx;
use ark_scene::merge::{FragmentRole, LoadedFragment, load_composition};
use ark_scene::parse::parse_scene;
use ark_scene::path::{DEFAULT_APPNAME, ResolvedScene, resolve_scene_path_from_env};

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene explain`.
#[derive(Debug, Args)]
#[command(
    about = "Trace resolution of a single ref across the composed scene",
    long_about = "Given a ref like `intent:<name>`, `keybind:<chord>`,\n\
                  `plugin:<name>`, `reaction:<selector>`, or `ext:<name>`,\n\
                  print every fragment that defined the ref plus the\n\
                  final merge-resolved origin.\n\
                  \n\
                  Examples:\n  \
                  ark scene explain intent:picker.show\n  \
                  ark scene explain keybind:'Alt p'\n  \
                  ark scene explain plugin:picker\n  \
                  ark scene explain reaction:Started\n  \
                  ark scene explain ext:aider-adapter"
)]
pub struct ExplainArgs {
    /// Ref to explain. Forms: `intent:<name>`, `keybind:<chord>`,
    /// `plugin:<name>`, `reaction:<selector>`, `ext:<name>`.
    #[arg(required = true, value_name = "REF")]
    pub reference: String,

    /// Path to a scene file. Uses the default scene when omitted.
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,
}

/// Parsed form of the user's ref argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    /// `intent:<name>` — dispatches an intent identifier.
    Intent(String),
    /// `keybind:<chord>` — key chord binding (R5 last-wins).
    Keybind(String),
    /// `plugin:<name>` — zellij wasm plugin declaration (R6).
    Plugin(String),
    /// `reaction:<selector>` — `on "<selector>"` reaction (R4).
    Reaction(String),
    /// `ext:<name>` — everything an extension contributed.
    Ext(String),
}

impl Ref {
    /// Short category label used in headers.
    fn category(&self) -> &'static str {
        match self {
            Ref::Intent(_) => "intent",
            Ref::Keybind(_) => "keybind",
            Ref::Plugin(_) => "plugin",
            Ref::Reaction(_) => "reaction",
            Ref::Ext(_) => "ext",
        }
    }

    /// The ref's payload (everything after the `<category>:` prefix).
    fn value(&self) -> &str {
        match self {
            Ref::Intent(v)
            | Ref::Keybind(v)
            | Ref::Plugin(v)
            | Ref::Reaction(v)
            | Ref::Ext(v) => v,
        }
    }
}

/// Parse a `<category>:<value>` ref specifier.
///
/// Whitespace inside `<value>` is preserved verbatim so chord refs like
/// `keybind:Alt p` work without shell-quoting tricks. Missing value or
/// unknown category produces a user-facing error string.
pub fn parse_ref(raw: &str) -> Result<Ref, String> {
    let (prefix, rest) = raw
        .split_once(':')
        .ok_or_else(|| format!(
            "missing ref prefix in `{raw}` (expected `intent:`, `keybind:`, \
             `plugin:`, `reaction:`, or `ext:`)"
        ))?;
    if rest.is_empty() {
        return Err(format!("empty ref value after `{prefix}:`"));
    }
    let value = rest.to_string();
    match prefix {
        "intent" => Ok(Ref::Intent(value)),
        "keybind" => Ok(Ref::Keybind(value)),
        "plugin" => Ok(Ref::Plugin(value)),
        "reaction" => Ok(Ref::Reaction(value)),
        "ext" => Ok(Ref::Ext(value)),
        other => Err(format!(
            "unknown ref category `{other}:` (expected `intent:`, `keybind:`, \
             `plugin:`, `reaction:`, or `ext:`)"
        )),
    }
}

/// Dispatch handler for `ark scene explain`.
pub fn run(args: ExplainArgs, _ctx: &Ctx) -> Result<(), CliError> {
    let reference = parse_ref(&args.reference).map_err(|reason| CliError::Generic {
        reason: format!("scene/explain: {reason}"),
    })?;

    // ---- Load scene source + composition --------------------------------
    let (src, entry_path, display_path) = load_scene_source(args.file.as_deref())?;
    let entry_doc =
        parse_scene(&src, entry_path.as_path()).map_err(|e| CliError::Generic {
            reason: format!("parse {display_path}: {e}"),
        })?;
    let search_ctx = build_search_ctx(&entry_path);
    let fragments = load_composition(entry_doc, entry_path.clone(), &search_ctx)
        .map_err(|e| CliError::Generic {
            reason: format!("resolve composition for {display_path}: {e}"),
        })?;

    // ---- Collect contributions matching the ref -------------------------
    let report = build_report(&reference, &fragments);

    // ---- Render ---------------------------------------------------------
    render(&reference, &report, &display_path);
    Ok(())
}

// ---------------------------------------------------------------------------
// Report model
// ---------------------------------------------------------------------------

/// A single contribution: one fragment declares one matching declaration.
#[derive(Debug, Clone)]
struct Hit {
    /// Fragment index into the composition (R11 load order).
    fragment_idx: usize,
    /// Display path of the fragment (e.g. `/path/to/base.kdl`).
    fragment_path: String,
    /// Role: `extends`, `include`, or `root`.
    fragment_role: &'static str,
    /// 1-based line number of the declaration in its fragment. `0` when
    /// the span could not be recovered (synthetic built-in, etc.).
    line: u32,
    /// Secondary payload shown inline (chord → intent name, plugin →
    /// source URI, keybind → target intent, ext → kind of contribution).
    detail: Option<String>,
}

/// The full explain report for one ref.
#[derive(Debug)]
struct Report {
    /// Every fragment that contributed a matching declaration, in R11
    /// load order.
    hits: Vec<Hit>,
    /// Index (in `hits`) of the R11 merge-winning declaration. `None`
    /// when the category is append-only (every hit survives: reactions,
    /// ext uses).
    winner: Option<usize>,
    /// `true` when the ref is category-append: every hit is part of the
    /// final scene (reactions + ext contributions).
    append_only: bool,
}

// ---------------------------------------------------------------------------
// Report building
// ---------------------------------------------------------------------------

fn build_report(reference: &Ref, fragments: &[LoadedFragment]) -> Report {
    match reference {
        Ref::Intent(name) => build_intent_report(name, fragments),
        Ref::Keybind(chord) => build_keybind_report(chord, fragments),
        Ref::Plugin(name) => build_plugin_report(name, fragments),
        Ref::Reaction(selector) => build_reaction_report(selector, fragments),
        Ref::Ext(name) => build_ext_report(name, fragments),
    }
}

/// Intent hits: every `keybind … intent="<name>"` declaration.
///
/// Merge rule: intents are dispatched via keybinds (R5) + ops — the
/// "winner" for a given intent name is the last-wins chord declaration
/// that names it. If two chords map to the same intent, both survive
/// (distinct chords), so intent resolution lists them all without a
/// single winner.
fn build_intent_report(name: &str, fragments: &[LoadedFragment]) -> Report {
    let mut hits = Vec::new();
    for (idx, frag) in fragments.iter().enumerate() {
        let raw_doc = load_raw_doc(&frag.path);
        for kb in &frag.doc.scene.keybinds {
            if kb.intent.as_deref() == Some(name) {
                let line = raw_doc
                    .as_ref()
                    .and_then(|d| find_node_line(d, "keybind", &[&kb.chord]))
                    .unwrap_or(0);
                hits.push(Hit {
                    fragment_idx: idx,
                    fragment_path: frag.path.display().to_string(),
                    fragment_role: role_label(frag.role),
                    line,
                    detail: Some(format!("via keybind \"{}\"", kb.chord)),
                });
            }
        }
    }
    // An intent is not itself last-wins: distinct chords in scope may
    // each dispatch it. No single winner unless the ref maps to exactly
    // one surviving keybind.
    Report {
        hits,
        winner: None,
        append_only: true,
    }
}

/// Keybind hits: every `keybind "<chord>"` declaration. R11 last-wins
/// per chord: the last fragment declaring the chord is the winner.
/// `clear-keybinds "<chord>"` from the root fragment wipes the winner.
fn build_keybind_report(chord: &str, fragments: &[LoadedFragment]) -> Report {
    let root_idx = root_index(fragments);
    let mut hits = Vec::new();
    for (idx, frag) in fragments.iter().enumerate() {
        let raw_doc = load_raw_doc(&frag.path);
        for kb in &frag.doc.scene.keybinds {
            if kb.chord == chord {
                let line = raw_doc
                    .as_ref()
                    .and_then(|d| find_node_line(d, "keybind", &[&kb.chord]))
                    .unwrap_or(0);
                let detail = kb.intent.as_ref().map(|i| format!("intent=\"{i}\""));
                hits.push(Hit {
                    fragment_idx: idx,
                    fragment_path: frag.path.display().to_string(),
                    fragment_role: role_label(frag.role),
                    line,
                    detail,
                });
            }
        }
    }

    // Root-level clear-keybinds directives erase the winner.
    let cleared_by_root = fragments
        .get(root_idx)
        .map(|root| {
            root.doc
                .scene
                .clear_keybinds
                .iter()
                .any(|c| c.chord == chord)
        })
        .unwrap_or(false);

    let winner = if cleared_by_root {
        None
    } else {
        // Last-wins: the largest fragment_idx among contributions wins.
        hits.iter()
            .enumerate()
            .max_by_key(|(_, h)| h.fragment_idx)
            .map(|(idx, _)| idx)
    };

    Report {
        hits,
        winner,
        append_only: false,
    }
}

/// Plugin hits: every `plugin "<name>" { … }` declaration. R11 merge:
/// duplicate-by-name is an error unless the later fragment set
/// `override=#true`; otherwise the last override wins. `disable-plugin`
/// from root drops every contribution.
fn build_plugin_report(name: &str, fragments: &[LoadedFragment]) -> Report {
    let root_idx = root_index(fragments);
    let mut hits = Vec::new();
    let mut override_flags = Vec::<bool>::new();
    for (idx, frag) in fragments.iter().enumerate() {
        let raw_doc = load_raw_doc(&frag.path);
        for p in &frag.doc.scene.plugins {
            if p.name == name {
                let line = raw_doc
                    .as_ref()
                    .and_then(|d| find_node_line(d, "plugin", &[&p.name]))
                    .unwrap_or(0);
                let detail = p.source.as_ref().map(|s| format!("source {}", s.uri));
                hits.push(Hit {
                    fragment_idx: idx,
                    fragment_path: frag.path.display().to_string(),
                    fragment_role: role_label(frag.role),
                    line,
                    detail,
                });
                override_flags.push(p.override_.unwrap_or(false));
            }
        }
    }

    // disable-plugin root directive drops everything.
    let disabled_by_root = fragments
        .get(root_idx)
        .map(|root| {
            root.doc
                .scene
                .disable_plugins
                .iter()
                .any(|d| d.name == name)
        })
        .unwrap_or(false);

    let winner = if disabled_by_root {
        None
    } else if hits.len() == 1 {
        Some(0)
    } else {
        // Walk: later with override=#true wins; otherwise earliest
        // conflict-free declaration holds.
        let mut current: Option<usize> = None;
        for i in 0..hits.len() {
            match current {
                None => current = Some(i),
                Some(_) if override_flags[i] => current = Some(i),
                Some(_) => { /* duplicate without override — merge conflict */ }
            }
        }
        current
    };

    Report {
        hits,
        winner,
        append_only: false,
    }
}

/// Reaction hits: every `on "<selector>"` declaration. R11 append-only:
/// every reaction survives in load order. `clear-reactions
/// selector="<sel>"` from the root drops prior matches.
fn build_reaction_report(selector: &str, fragments: &[LoadedFragment]) -> Report {
    let root_idx = root_index(fragments);
    let mut hits = Vec::new();
    for (idx, frag) in fragments.iter().enumerate() {
        let raw_doc = load_raw_doc(&frag.path);
        for on in &frag.doc.scene.ons {
            if on.selector == selector {
                let line = raw_doc
                    .as_ref()
                    .and_then(|d| find_node_line(d, "on", &[&on.selector]))
                    .unwrap_or(0);
                let detail = on.if_.as_ref().map(|c| format!("if=\"{c}\""));
                hits.push(Hit {
                    fragment_idx: idx,
                    fragment_path: frag.path.display().to_string(),
                    fragment_role: role_label(frag.role),
                    line,
                    detail,
                });
            }
        }
    }

    let cleared_by_root = fragments
        .get(root_idx)
        .map(|root| {
            root.doc
                .scene
                .clear_reactions
                .iter()
                .any(|c| c.selector == selector)
        })
        .unwrap_or(false);

    // Reactions are append-only except for root-clear, which drops
    // everything matching the selector that appeared before root.
    let hits = if cleared_by_root {
        hits.into_iter()
            .filter(|h| h.fragment_idx >= root_idx)
            .collect()
    } else {
        hits
    };

    Report {
        hits,
        winner: None,
        append_only: true,
    }
}

/// Ext hits: every contribution attributable to the named extension.
/// v1 covers the direct signals:
///
/// * `use "<name>"` activations (one hit per fragment).
/// * `plugin { source "ext:<name>" }` declarations.
/// * `keybind … intent="<name>.<…>"` — best-effort: when the intent
///   prefix matches the ext name (e.g. `ext:picker` + `intent="picker.show"`),
///   the keybind is listed as a likely contribution. This is a heuristic
///   the extension author controls via their intent namespace.
fn build_ext_report(name: &str, fragments: &[LoadedFragment]) -> Report {
    let mut hits = Vec::new();
    let intent_prefix = format!("{name}.");

    for (idx, frag) in fragments.iter().enumerate() {
        let raw_doc = load_raw_doc(&frag.path);

        // `use "<name>"`
        for u in &frag.doc.scene.uses {
            if u.name == name {
                let line = raw_doc
                    .as_ref()
                    .and_then(|d| find_node_line(d, "use", &[&u.name]))
                    .unwrap_or(0);
                hits.push(Hit {
                    fragment_idx: idx,
                    fragment_path: frag.path.display().to_string(),
                    fragment_role: role_label(frag.role),
                    line,
                    detail: Some(format!("use \"{}\"", u.name)),
                });
            }
        }

        // `plugin { source "ext:<name>" }`
        for p in &frag.doc.scene.plugins {
            if let Some(src) = &p.source {
                let matches_ext = src.uri == format!("ext:{name}");
                if matches_ext {
                    let line = raw_doc
                        .as_ref()
                        .and_then(|d| find_node_line(d, "plugin", &[&p.name]))
                        .unwrap_or(0);
                    hits.push(Hit {
                        fragment_idx: idx,
                        fragment_path: frag.path.display().to_string(),
                        fragment_role: role_label(frag.role),
                        line,
                        detail: Some(format!("plugin \"{}\" source=ext:{name}", p.name)),
                    });
                }
            }
        }

        // Keybinds dispatching an intent namespaced under the ext name.
        for kb in &frag.doc.scene.keybinds {
            if let Some(intent) = &kb.intent {
                if intent == name || intent.starts_with(&intent_prefix) {
                    let line = raw_doc
                        .as_ref()
                        .and_then(|d| find_node_line(d, "keybind", &[&kb.chord]))
                        .unwrap_or(0);
                    hits.push(Hit {
                        fragment_idx: idx,
                        fragment_path: frag.path.display().to_string(),
                        fragment_role: role_label(frag.role),
                        line,
                        detail: Some(format!(
                            "keybind \"{}\" intent=\"{intent}\"",
                            kb.chord
                        )),
                    });
                }
            }
        }
    }

    Report {
        hits,
        winner: None,
        append_only: true,
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(reference: &Ref, report: &Report, scene_display: &str) {
    println!(
        "scene explain {}:{}  (entry: {})",
        reference.category(),
        reference.value(),
        scene_display
    );
    println!();

    if report.hits.is_empty() {
        println!(
            "no fragment declares {}:{} in the composed scene",
            reference.category(),
            reference.value()
        );
        return;
    }

    let last_idx = report.hits.len().saturating_sub(1);
    for (idx, hit) in report.hits.iter().enumerate() {
        let verb = if idx == 0 {
            "defined at"
        } else {
            "overridden by"
        };
        let line_tag = if hit.line > 0 {
            format!(":{}", hit.line)
        } else {
            String::new()
        };
        let detail = hit
            .detail
            .as_deref()
            .map(|d| format!("  [{d}]"))
            .unwrap_or_default();
        println!(
            "  {verb} {path}{line} ({role}){detail}",
            path = hit.fragment_path,
            line = line_tag,
            role = hit.fragment_role,
        );
        if idx == last_idx {
            // Last line already printed; nothing further to append per-hit.
        }
    }

    println!();
    if report.append_only {
        match reference {
            Ref::Reaction(_) => {
                println!(
                    "final resolution: append-only — all {} reaction(s) retained in load order",
                    report.hits.len()
                );
            }
            Ref::Intent(_) => {
                println!(
                    "final resolution: append-only — {} keybind(s) dispatch this intent",
                    report.hits.len()
                );
            }
            Ref::Ext(_) => {
                println!(
                    "final resolution: extension contributed {} entr{} (see above)",
                    report.hits.len(),
                    if report.hits.len() == 1 { "y" } else { "ies" }
                );
            }
            _ => {
                println!(
                    "final resolution: {} contribution(s) retained",
                    report.hits.len()
                );
            }
        }
    } else {
        match report.winner {
            Some(w) => {
                let hit = &report.hits[w];
                let line_tag = if hit.line > 0 {
                    format!(":{}", hit.line)
                } else {
                    String::new()
                };
                println!(
                    "final resolution: {path}{line} ({role})",
                    path = hit.fragment_path,
                    line = line_tag,
                    role = hit.fragment_role
                );
            }
            None => {
                println!(
                    "final resolution: no surviving declaration (cleared or disabled by root)"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn role_label(role: FragmentRole) -> &'static str {
    match role {
        FragmentRole::Extends => "extends",
        FragmentRole::Include => "include",
        FragmentRole::Root => "root",
    }
}

fn root_index(fragments: &[LoadedFragment]) -> usize {
    fragments
        .iter()
        .position(|f| f.role == FragmentRole::Root)
        .unwrap_or(fragments.len().saturating_sub(1))
}

fn load_raw_doc(path: &Path) -> Option<KdlDocument> {
    let src = std::fs::read_to_string(path).ok()?;
    KdlDocument::parse(&src).ok()
}

/// Find the 1-based line number of a KDL node named `name` inside the
/// top-level `scene { … }` block whose first positional argument matches
/// `args[0]` (when supplied). Mirrors the graph command's helper so both
/// subcommands agree on span attribution.
fn find_node_line(doc: &KdlDocument, name: &str, args: &[&str]) -> Option<u32> {
    for top in doc.nodes() {
        if top.name().value() != "scene" {
            continue;
        }
        let Some(children) = top.children() else {
            continue;
        };
        for child in children.nodes() {
            if child.name().value() != name {
                continue;
            }
            if !args.is_empty() {
                let first = child
                    .entries()
                    .iter()
                    .find(|e| e.name().is_none())
                    .and_then(|e| e.value().as_string());
                if first != Some(args[0]) {
                    continue;
                }
            }
            let offset = child.span().offset();
            let doc_text = doc.to_string();
            let line = offset_to_line(&doc_text, offset);
            return Some(line);
        }
    }
    None
}

fn offset_to_line(src: &str, offset: usize) -> u32 {
    let mut line: u32 = 1;
    for (i, ch) in src.char_indices() {
        if i >= offset {
            return line;
        }
        if ch == '\n' {
            line += 1;
        }
    }
    line
}

// ---------------------------------------------------------------------------
// Scene source loading (shared contract with `scene graph`)
// ---------------------------------------------------------------------------

fn load_scene_source(
    explicit: Option<&Path>,
) -> Result<(String, PathBuf, String), CliError> {
    if let Some(path) = explicit {
        let src = std::fs::read_to_string(path).map_err(|e| CliError::Generic {
            reason: format!("cannot read {}: {e}", path.display()),
        })?;
        Ok((src, path.to_path_buf(), path.display().to_string()))
    } else {
        let cwd = std::env::current_dir().map_err(|e| CliError::Generic {
            reason: format!("cannot determine cwd: {e}"),
        })?;
        match resolve_scene_path_from_env(None, &cwd) {
            ResolvedScene::Named(name) => Err(CliError::Generic {
                reason: format!(
                    "scene `{name}` resolved by name; pass `--file <PATH>` to `ark scene explain`"
                ),
            }),
            ResolvedScene::Path(p) => {
                let src = std::fs::read_to_string(&p).map_err(|e| CliError::Generic {
                    reason: format!("cannot read {}: {e}", p.display()),
                })?;
                let display = p.display().to_string();
                Ok((src, p, display))
            }
            ResolvedScene::BuiltIn(src) => Ok((
                src.to_string(),
                PathBuf::from("<built-in>"),
                "<built-in>".to_string(),
            )),
        }
    }
}

fn build_search_ctx(entry_path: &Path) -> SceneSearchCtx {
    let cwd = entry_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let xdg = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config"))
        });
    let appname = std::env::var("ARK_APPNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_APPNAME.to_string());
    SceneSearchCtx {
        cwd,
        xdg_config_home: xdg,
        appname,
        builtins: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // --- ref parsing ---

    #[test]
    fn parse_intent_ref() {
        assert_eq!(parse_ref("intent:picker.show").unwrap(), Ref::Intent("picker.show".into()));
    }

    #[test]
    fn parse_keybind_ref_preserves_whitespace() {
        assert_eq!(parse_ref("keybind:Alt p").unwrap(), Ref::Keybind("Alt p".into()));
    }

    #[test]
    fn parse_plugin_ref() {
        assert_eq!(parse_ref("plugin:picker").unwrap(), Ref::Plugin("picker".into()));
    }

    #[test]
    fn parse_reaction_ref() {
        assert_eq!(parse_ref("reaction:Started").unwrap(), Ref::Reaction("Started".into()));
    }

    #[test]
    fn parse_ext_ref() {
        assert_eq!(parse_ref("ext:aider").unwrap(), Ref::Ext("aider".into()));
    }

    #[test]
    fn parse_ref_rejects_missing_colon() {
        let err = parse_ref("pickerOnly").unwrap_err();
        assert!(err.contains("missing ref prefix"), "{err}");
    }

    #[test]
    fn parse_ref_rejects_empty_value() {
        let err = parse_ref("intent:").unwrap_err();
        assert!(err.contains("empty ref value"), "{err}");
    }

    #[test]
    fn parse_ref_rejects_unknown_category() {
        let err = parse_ref("engine:claude").unwrap_err();
        assert!(err.contains("unknown ref category"), "{err}");
    }

    // --- fragment setup helpers ---

    fn setup_extends_scene() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"scene "base" {
    use "picker"
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
    }
    keybind "Alt p" intent="picker.show"
    on "Started" { }
}
"#,
        )
        .unwrap();

        let child_path = root.join("child.kdl");
        fs::write(
            &child_path,
            r#"scene "child" {
    extends "base"
    keybind "Alt p" intent="user.custom"
    on "Done" { }
}
"#,
        )
        .unwrap();
        (tmp, child_path)
    }

    fn composition(path: &Path) -> Vec<LoadedFragment> {
        let src = fs::read_to_string(path).unwrap();
        let doc = parse_scene(&src, path).unwrap();
        let ctx = SceneSearchCtx::new(path.parent().unwrap());
        load_composition(doc, path.to_path_buf(), &ctx).unwrap()
    }

    // --- keybind last-wins ---

    #[test]
    fn keybind_report_picks_child_winner() {
        let (_tmp, child) = setup_extends_scene();
        let frags = composition(&child);
        let report = build_keybind_report("Alt p", &frags);
        assert_eq!(report.hits.len(), 2);
        assert_eq!(report.winner, Some(1), "last fragment wins");
    }

    #[test]
    fn keybind_report_empty_when_chord_missing() {
        let (_tmp, child) = setup_extends_scene();
        let frags = composition(&child);
        let report = build_keybind_report("Ctrl q", &frags);
        assert!(report.hits.is_empty());
        assert!(report.winner.is_none());
    }

    // --- plugin override ---

    #[test]
    fn plugin_report_override_wins() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"scene "base" {
    plugin "status" {
        source "shipped:status"
        mount "status-bar"
    }
}
"#,
        )
        .unwrap();
        let child_path = root.join("child.kdl");
        fs::write(
            &child_path,
            r#"scene "child" {
    extends "base"
    plugin "status" override=#true {
        source "shipped:status"
        mount "floating"
    }
}
"#,
        )
        .unwrap();
        let frags = composition(&child_path);
        let report = build_plugin_report("status", &frags);
        assert_eq!(report.hits.len(), 2);
        assert_eq!(report.winner, Some(1), "override=#true wins");
    }

    #[test]
    fn plugin_report_disabled_by_root_has_no_winner() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"scene "base" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
    }
}
"#,
        )
        .unwrap();
        let child_path = root.join("child.kdl");
        fs::write(
            &child_path,
            r#"scene "child" {
    extends "base"
    disable-plugins "picker"
}
"#,
        )
        .unwrap();
        let frags = composition(&child_path);
        let report = build_plugin_report("picker", &frags);
        assert_eq!(report.hits.len(), 1);
        assert!(report.winner.is_none());
    }

    // --- reactions append ---

    #[test]
    fn reaction_report_lists_all_matching_selectors() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"scene "base" {
    on "Started" { }
}
"#,
        )
        .unwrap();
        let child_path = root.join("child.kdl");
        fs::write(
            &child_path,
            r#"scene "child" {
    extends "base"
    on "Started" { }
}
"#,
        )
        .unwrap();
        let frags = composition(&child_path);
        let report = build_reaction_report("Started", &frags);
        assert_eq!(report.hits.len(), 2);
        assert!(report.append_only);
    }

    #[test]
    fn reaction_report_root_clear_drops_parents() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scenes_dir = root.join(".ark/scenes");
        fs::create_dir_all(&scenes_dir).unwrap();
        fs::write(
            scenes_dir.join("base.kdl"),
            r#"scene "base" {
    on "Started" { }
}
"#,
        )
        .unwrap();
        let child_path = root.join("child.kdl");
        fs::write(
            &child_path,
            r#"scene "child" {
    extends "base"
    clear-reactions selector="Started"
}
"#,
        )
        .unwrap();
        let frags = composition(&child_path);
        let report = build_reaction_report("Started", &frags);
        // Parent reaction dropped; no root contribution either.
        assert!(report.hits.is_empty());
    }

    // --- intent ---

    #[test]
    fn intent_report_lists_keybinds_dispatching_it() {
        let (_tmp, child) = setup_extends_scene();
        let frags = composition(&child);
        let report = build_intent_report("picker.show", &frags);
        // Only base has intent="picker.show" (child's Alt p re-maps to
        // user.custom).
        assert_eq!(report.hits.len(), 1);
        assert_eq!(report.hits[0].fragment_idx, 0);
    }

    // --- ext ---

    #[test]
    fn ext_report_collects_uses_and_namespaced_intents() {
        let (_tmp, child) = setup_extends_scene();
        let frags = composition(&child);
        let report = build_ext_report("picker", &frags);
        // Base: `use "picker"` + keybind intent="picker.show" = 2 hits.
        assert!(
            report.hits.len() >= 2,
            "expected at least 2 hits, got {}",
            report.hits.len()
        );
        assert!(report.append_only);
    }

    // --- find_node_line ---

    #[test]
    fn find_node_line_locates_plugin() {
        let src = r#"scene "demo" {
    plugin "picker" {
        source "shipped:picker"
    }
}
"#;
        let doc = KdlDocument::parse(src).unwrap();
        let line = find_node_line(&doc, "plugin", &["picker"]).unwrap();
        assert!(line >= 2);
    }

    #[test]
    fn offset_to_line_counts_newlines() {
        let src = "a\nbb\nccc\n";
        assert_eq!(offset_to_line(src, 0), 1);
        assert_eq!(offset_to_line(src, 2), 2);
        assert_eq!(offset_to_line(src, 5), 3);
    }
}
