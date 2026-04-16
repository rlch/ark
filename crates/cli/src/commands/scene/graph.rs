//! `ark scene graph` — render attribution tree.
//!
//! T-12.5 (cavekit-scene R13). Shows extensions, plugins, reactions,
//! keybinds, intents — each leaf tagged with origin file:line.
//!
//! The command:
//!
//! 1. Resolves the scene source (explicit path or default resolver).
//! 2. Parses the entry scene into a [`SceneDoc`].
//! 3. Walks the composition graph (`extends` + `include` chains) so
//!    every fragment contributing to the final scene is enumerated.
//! 4. For each contribution (plugin, reaction, keybind, extension-use),
//!    computes a `file:line` origin by re-parsing the owning fragment's
//!    raw KDL and reading span offsets.
//! 5. Prints an ASCII tree (default) or a JSON object (`--format json`)
//!    the same shape a future `ark-lsp` surface can consume.
//!
//! Graph output is intentionally coarse: the first occurrence of each
//! plugin/keybind/reaction body within its fragment is attributed — if
//! the user wants full per-merge provenance, `ark scene explain-merge`
//! is the more surgical tool.

use std::path::{Path, PathBuf};

use clap::Args;
use kdl::KdlDocument;
use serde_json::{Value, json};

use ark_scene::ast::SceneDoc;
use ark_scene::extends::SceneSearchCtx;
use ark_scene::merge::{FragmentRole, LoadedFragment, load_composition};
use ark_scene::parse::parse_scene;
use ark_scene::path::{DEFAULT_APPNAME, ResolvedScene, resolve_scene_path_from_env};

use crate::ctx::Ctx;
use crate::error::CliError;

/// Arguments for `ark scene graph`.
#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Path to a scene file. Graphs the default scene when omitted.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Output format: `text` (ASCII tree) or `json` (for scripts + future lsp).
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    pub format: String,
}

pub fn run(args: GraphArgs, _ctx: &Ctx) -> Result<(), CliError> {
    // ---- Step 1: resolve + load scene source ----------------------------
    let (src, entry_path, display_path) = load_scene_source(args.path.as_deref())?;

    // ---- Step 2: parse the entry document -------------------------------
    let entry_doc =
        parse_scene(&src, entry_path.as_path()).map_err(|e| CliError::Generic {
            reason: format!("parse {display_path}: {e}"),
        })?;

    // ---- Step 3: walk the composition graph ----------------------------
    //
    // `load_composition` follows every `extends` and `include` edge, so
    // the returned Vec<LoadedFragment> enumerates every file that
    // contributes to the final scene. Failures here are surfaced as
    // generic errors — the resolver's miette source is forwarded
    // verbatim.
    let search_ctx = build_search_ctx(&entry_path);
    let fragments = load_composition(entry_doc, entry_path.clone(), &search_ctx)
        .map_err(|e| CliError::Generic {
            reason: format!("resolve composition for {display_path}: {e}"),
        })?;

    // ---- Step 4: build the graph model ---------------------------------
    let graph = build_graph_model(&fragments, &entry_path)?;

    // ---- Step 5: render --------------------------------------------------
    match args.format.as_str() {
        "json" => render_json(&graph),
        _ => render_text(&graph, &display_path),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Scene source loading
// ---------------------------------------------------------------------------

/// Load the scene source + the path it came from.
///
/// Named scenes (`--scene NAME`) are rejected with a clear message: the
/// graph command operates on files, not symbolic names. Built-in
/// defaults surface as a synthetic `<built-in>` path so downstream
/// span-attribution still has a key to print.
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
                    "scene `{name}` resolved by name; pass a path to `ark scene graph`"
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

/// Build a [`SceneSearchCtx`] seeded from the entry scene's directory so
/// `extends "<name>"` probes walk the same rungs the runtime uses.
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
// Graph model
// ---------------------------------------------------------------------------

/// Flattened attribution tree — one entry per fragment, plus the list
/// of contributions the fragment brings to the final scene.
#[derive(Debug)]
struct GraphModel {
    /// Display path of the entry scene (for headers).
    entry_display: String,
    /// Logical scene name (`scene "<name>"`) from the root fragment.
    scene_name: String,
    /// Each contributing fragment in R11 load order.
    fragments: Vec<FragmentEntry>,
}

/// One fragment — a file (or synthetic built-in) that contributed nodes
/// to the composed scene.
#[derive(Debug)]
struct FragmentEntry {
    /// Role: `Extends` parent, `Include` splice, or `Root` (the entry).
    role: String,
    /// Path the fragment was loaded from (display form).
    path: String,
    /// `scene "<name>"` declaration inside this fragment.
    scene_name: String,
    /// Extensions this fragment activates (`use "<name>"`).
    uses: Vec<Contribution>,
    /// Plugins this fragment declares (`plugin "<name>"`).
    plugins: Vec<Contribution>,
    /// Reactions this fragment declares (`on "<selector>"`).
    reactions: Vec<Contribution>,
    /// Keybinds this fragment declares (`keybind "<chord>"`).
    keybinds: Vec<Contribution>,
    /// Intents exposed via `keybind intent=…`. Surfaced so the tree
    /// lists every intent identifier an author might want to audit.
    intents: Vec<Contribution>,
}

/// One contribution — label + `file:line` origin tag.
#[derive(Debug, Clone)]
struct Contribution {
    /// Display name (e.g. plugin name, chord string, selector).
    label: String,
    /// 1-based line number within the owning fragment's KDL source.
    /// `0` when the origin could not be recovered (malformed raw KDL or
    /// synthetic built-in).
    line: u32,
    /// Optional secondary tag (e.g. keybind → intent, plugin → source URI).
    detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Model building
// ---------------------------------------------------------------------------

fn build_graph_model(
    fragments: &[LoadedFragment],
    entry_path: &Path,
) -> Result<GraphModel, CliError> {
    let mut entries = Vec::with_capacity(fragments.len());
    let mut scene_name = String::new();

    for frag in fragments {
        if matches!(frag.role, FragmentRole::Root) {
            scene_name = frag.doc.scene.name.clone();
        }
        entries.push(build_fragment_entry(frag)?);
    }

    Ok(GraphModel {
        entry_display: entry_path.display().to_string(),
        scene_name,
        fragments: entries,
    })
}

/// Build a single fragment's graph entry.
///
/// We re-read the fragment's KDL source to recover span / line-number
/// attribution for each contribution. Re-parse failures degrade to
/// `line=0` rather than aborting the whole command — the typed AST has
/// already been accepted upstream, and a graph without spans still
/// conveys authoritative structure.
fn build_fragment_entry(frag: &LoadedFragment) -> Result<FragmentEntry, CliError> {
    let raw_src = std::fs::read_to_string(&frag.path).ok();
    let raw_doc = raw_src
        .as_deref()
        .and_then(|s| KdlDocument::parse(s).ok());

    let uses = frag
        .doc
        .scene
        .uses
        .iter()
        .map(|u| Contribution {
            label: u.name.clone(),
            line: raw_doc
                .as_ref()
                .and_then(|d| find_node_line(d, "use", &[&u.name]))
                .unwrap_or(0),
            detail: None,
        })
        .collect();

    let plugins = frag
        .doc
        .scene
        .plugins
        .iter()
        .map(|p| Contribution {
            label: p.name.clone(),
            line: raw_doc
                .as_ref()
                .and_then(|d| find_node_line(d, "plugin", &[&p.name]))
                .unwrap_or(0),
            detail: p.source.as_ref().map(|s| s.uri.clone()),
        })
        .collect();

    let reactions = frag
        .doc
        .scene
        .ons
        .iter()
        .map(|o| Contribution {
            label: o.selector.clone(),
            line: raw_doc
                .as_ref()
                .and_then(|d| find_node_line(d, "on", &[&o.selector]))
                .unwrap_or(0),
            detail: o.if_.as_ref().map(|s| format!("if=\"{s}\"")),
        })
        .collect();

    let keybinds: Vec<Contribution> = frag
        .doc
        .scene
        .keybinds
        .iter()
        .map(|k| Contribution {
            label: k.chord.clone(),
            line: raw_doc
                .as_ref()
                .and_then(|d| find_node_line(d, "keybind", &[&k.chord]))
                .unwrap_or(0),
            detail: k.intent.as_ref().map(|s| format!("intent={s}")),
        })
        .collect();

    // Intents are a derived view of keybinds: every non-empty
    // `intent="<name>"` contributes one identifier worth graphing.
    let intents = frag
        .doc
        .scene
        .keybinds
        .iter()
        .filter_map(|k| {
            k.intent.as_ref().map(|name| Contribution {
                label: name.clone(),
                line: raw_doc
                    .as_ref()
                    .and_then(|d| find_node_line(d, "keybind", &[&k.chord]))
                    .unwrap_or(0),
                detail: Some(format!("from keybind \"{}\"", k.chord)),
            })
        })
        .collect();

    Ok(FragmentEntry {
        role: fragment_role_label(frag.role).to_string(),
        path: frag.path.display().to_string(),
        scene_name: frag.doc.scene.name.clone(),
        uses,
        plugins,
        reactions,
        keybinds,
        intents,
    })
}

fn fragment_role_label(role: FragmentRole) -> &'static str {
    match role {
        FragmentRole::Extends => "extends",
        FragmentRole::Include => "include",
        FragmentRole::Root => "root",
    }
}

/// Find the 1-based line number of a KDL node named `name` inside the
/// top-level `scene { … }` block whose first positional argument matches
/// `args[0]` (when supplied).
///
/// Returns `None` when no matching node exists — callers degrade to
/// `line=0` for display purposes.
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
            // When args are supplied, require the first positional arg
            // to match. Avoids confusing e.g. two `plugin "picker"` vs
            // `plugin "status"` declarations at graph time.
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
            // `KdlNode::span()` returns a SourceSpan — count newlines
            // up to the span offset in the source to produce a
            // 1-based line number. We need the raw source; recover it
            // from the document's leading text + node spans. The
            // `KdlDocument::to_string()` roundtrip preserves byte
            // offsets cleanly.
            let offset = child.span().offset();
            // Walk the document text to compute a line from offset.
            let doc_text = doc.to_string();
            let line = offset_to_line(&doc_text, offset);
            return Some(line);
        }
    }
    None
}

/// Convert a byte offset into a 1-based line number.
///
/// Lines are delimited by `\n` (KDL 2.0 normalizes line endings). An
/// offset past the end of the source saturates to the last line + 1.
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
// Rendering
// ---------------------------------------------------------------------------

fn render_text(graph: &GraphModel, _display_path: &str) {
    println!(
        "scene \"{name}\" (entry: {path})",
        name = graph.scene_name,
        path = graph.entry_display
    );
    for (idx, frag) in graph.fragments.iter().enumerate() {
        let last = idx + 1 == graph.fragments.len();
        let top_prefix = if last { "└── " } else { "├── " };
        let inner_prefix = if last { "    " } else { "│   " };
        println!(
            "{top_prefix}[{role}] scene \"{name}\"  ({path})",
            role = frag.role,
            name = frag.scene_name,
            path = frag.path,
        );
        render_section(inner_prefix, "uses", &frag.uses);
        render_section(inner_prefix, "plugins", &frag.plugins);
        render_section(inner_prefix, "reactions", &frag.reactions);
        render_section(inner_prefix, "keybinds", &frag.keybinds);
        render_section(inner_prefix, "intents", &frag.intents);
    }
}

fn render_section(outer: &str, label: &str, items: &[Contribution]) {
    if items.is_empty() {
        return;
    }
    println!("{outer}├── {label}");
    let last_idx = items.len() - 1;
    for (i, c) in items.iter().enumerate() {
        let leaf = if i == last_idx { "└── " } else { "├── " };
        let detail = c
            .detail
            .as_deref()
            .map(|d| format!("  [{d}]"))
            .unwrap_or_default();
        let line_tag = if c.line > 0 {
            format!("  @ line {}", c.line)
        } else {
            String::new()
        };
        println!("{outer}│   {leaf}{label}{line_tag}{detail}", label = c.label);
    }
}

fn render_json(graph: &GraphModel) {
    let fragments: Vec<Value> = graph
        .fragments
        .iter()
        .map(|f| {
            json!({
                "role": f.role,
                "path": f.path,
                "scene_name": f.scene_name,
                "uses": contribs_json(&f.uses),
                "plugins": contribs_json(&f.plugins),
                "reactions": contribs_json(&f.reactions),
                "keybinds": contribs_json(&f.keybinds),
                "intents": contribs_json(&f.intents),
            })
        })
        .collect();
    let out = json!({
        "scene_name": graph.scene_name,
        "entry": graph.entry_display,
        "fragments": fragments,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

fn contribs_json(items: &[Contribution]) -> Value {
    let arr: Vec<Value> = items
        .iter()
        .map(|c| {
            json!({
                "label": c.label,
                "line": c.line,
                "detail": c.detail,
            })
        })
        .collect();
    Value::Array(arr)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).expect("write scene");
        path
    }

    #[test]
    fn offset_to_line_counts_newlines() {
        let src = "a\nbb\nccc\n";
        assert_eq!(offset_to_line(src, 0), 1);
        assert_eq!(offset_to_line(src, 2), 2);
        assert_eq!(offset_to_line(src, 5), 3);
        // Past-end saturates to final line.
        assert_eq!(offset_to_line(src, 999), 4);
    }

    #[test]
    fn find_node_line_locates_plugin_by_name() {
        let src = r#"scene "demo" {
    plugin "picker" {
        source "shipped:picker"
    }
    plugin "status" {
        source "shipped:status"
    }
}
"#;
        let doc = KdlDocument::parse(src).expect("parse");
        let picker_line = find_node_line(&doc, "plugin", &["picker"]).unwrap();
        let status_line = find_node_line(&doc, "plugin", &["status"]).unwrap();
        assert!(picker_line >= 2);
        assert!(status_line > picker_line);
    }

    #[test]
    fn build_graph_model_captures_root_fragment() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let scene = r#"scene "demo" {
    plugin "picker" {
        source "shipped:picker"
        mount "floating"
    }
    keybind "Alt p" intent="picker.show"
    on "Started" { }
}
"#;
        let path = write(&root, "scene.kdl", scene);
        let doc = parse_scene(scene, &path).expect("parse");
        let ctx = SceneSearchCtx::new(&root);
        let frags = load_composition(doc, path.clone(), &ctx).expect("load");
        let graph = build_graph_model(&frags, &path).expect("model");
        assert_eq!(graph.scene_name, "demo");
        assert_eq!(graph.fragments.len(), 1);
        let root_frag = &graph.fragments[0];
        assert_eq!(root_frag.role, "root");
        assert_eq!(root_frag.plugins.len(), 1);
        assert_eq!(root_frag.plugins[0].label, "picker");
        assert_eq!(root_frag.keybinds.len(), 1);
        assert_eq!(root_frag.keybinds[0].label, "Alt p");
        assert_eq!(root_frag.reactions.len(), 1);
        assert_eq!(root_frag.reactions[0].label, "Started");
        assert_eq!(root_frag.intents.len(), 1);
        assert_eq!(root_frag.intents[0].label, "picker.show");
    }

    #[test]
    fn build_graph_model_captures_extends_parent() {
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

        let child_src = r#"scene "child" {
    extends "base"
    keybind "Alt p" intent="picker.show"
}
"#;
        let child_path = write(&root, "child.kdl", child_src);
        let doc = parse_scene(child_src, &child_path).expect("parse");
        let ctx = SceneSearchCtx::new(&root);
        let frags = load_composition(doc, child_path.clone(), &ctx).expect("load");
        let graph = build_graph_model(&frags, &child_path).expect("model");
        assert_eq!(graph.fragments.len(), 2);
        assert_eq!(graph.fragments[0].role, "extends");
        assert_eq!(graph.fragments[0].plugins.len(), 1);
        assert_eq!(graph.fragments[1].role, "root");
        assert_eq!(graph.fragments[1].keybinds.len(), 1);
    }

    #[test]
    fn render_json_serialises_shape() {
        let graph = GraphModel {
            entry_display: "scene.kdl".to_string(),
            scene_name: "demo".to_string(),
            fragments: vec![FragmentEntry {
                role: "root".to_string(),
                path: "scene.kdl".to_string(),
                scene_name: "demo".to_string(),
                uses: vec![],
                plugins: vec![Contribution {
                    label: "picker".to_string(),
                    line: 2,
                    detail: Some("shipped:picker".to_string()),
                }],
                reactions: vec![],
                keybinds: vec![],
                intents: vec![],
            }],
        };
        // We can't capture stdout easily; assert the JSON structure via
        // the helper directly. The render function just prints this.
        let arr = contribs_json(&graph.fragments[0].plugins);
        assert_eq!(arr[0]["label"], Value::String("picker".into()));
        assert_eq!(arr[0]["line"], Value::Number(2.into()));
    }
}
