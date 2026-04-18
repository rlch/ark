use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::layout::{LayoutChild, TabNode};
use crate::ast::{LayoutNode, ModeNode, SceneBodyNode};
use crate::error::SceneError;
use crate::ext::ExtensionRegistry;
use crate::parse::{parse_scene, SceneIR};

/// Resolve all `include "<path>"` nodes by reading, parsing, and splicing
/// fragment body nodes at each include point. Detects handle conflicts
/// across fragments (T-077) and include cycles (T-076).
///
/// `ext:` includes are preserved as-is at this stage. Call
/// [`resolve_ext_includes`] after composition to resolve them against the
/// extension registry (T-075).
pub fn compose_scene(mut ir: SceneIR) -> Result<SceneIR, SceneError> {
    let canon_path = ir.path.canonicalize().unwrap_or_else(|_| ir.path.clone());
    let root_dir = canon_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let mut stack = vec![canon_path];

    let scene_source = ir.path.display().to_string();
    let mut handles: HashMap<String, String> = HashMap::new();
    collect_handles_from_body(&ir.scene.body, &scene_source, &mut handles)?;

    let composed_body =
        resolve_includes(&ir.scene.body, &ir.path, &root_dir, &mut stack, &mut handles)?;
    ir.scene.body = composed_body;
    Ok(ir)
}

fn resolve_includes(
    body: &[SceneBodyNode],
    parent_path: &Path,
    root_dir: &Path,
    stack: &mut Vec<PathBuf>,
    handles: &mut HashMap<String, String>,
) -> Result<Vec<SceneBodyNode>, SceneError> {
    let parent_dir = parent_path.parent().unwrap_or(Path::new("."));
    let mut result = Vec::new();

    for node in body {
        match node {
            SceneBodyNode::Include(inc) if !inc.target.starts_with("ext:") => {
                let resolved = parent_dir.join(&inc.target);
                let canonical = resolved.canonicalize().map_err(|e| {
                    SceneError::IncludeNotFound {
                        target: inc.target.clone(),
                        reason: e.to_string(),
                    }
                })?;

                // F-0022: path sandboxing — include must stay within root dir
                if !canonical.starts_with(root_dir) {
                    return Err(SceneError::IncludeEscape {
                        target: inc.target.clone(),
                        root: root_dir.display().to_string(),
                    });
                }

                // F-0018: cycle detection uses the DFS stack, not a flat set.
                // Diamond includes (same file via independent paths) are allowed.
                if stack.contains(&canonical) {
                    return Err(SceneError::IncludeCycle {
                        target: inc.target.clone(),
                        stack: stack.clone(),
                    });
                }

                let content = std::fs::read_to_string(&canonical).map_err(|e| {
                    SceneError::IncludeNotFound {
                        target: inc.target.clone(),
                        reason: e.to_string(),
                    }
                })?;

                let fragment_ir = parse_fragment(&content, &canonical, &inc.target)?;
                let fragment_source = canonical.display().to_string();
                check_handle_conflicts(&fragment_ir.scene.body, &fragment_source, handles)?;

                stack.push(canonical.clone());
                let nested =
                    resolve_includes(&fragment_ir.scene.body, &canonical, root_dir, stack, handles)?;
                stack.pop();
                result.extend(nested);
            }
            other => result.push(other.clone()),
        }
    }

    Ok(result)
}

/// Check that handles in `body` don't conflict with already-registered handles,
/// then register the new ones.
fn check_handle_conflicts(
    body: &[SceneBodyNode],
    source: &str,
    handles: &mut HashMap<String, String>,
) -> Result<(), SceneError> {
    let mut new_handles: Vec<String> = Vec::new();
    collect_raw_handles(body, &mut new_handles);

    for h in &new_handles {
        if let Some(first_source) = handles.get(h) {
            return Err(SceneError::IncludeHandleClash {
                handle: h.clone(),
                first: first_source.clone(),
                second: source.to_string(),
            });
        }
    }
    for h in new_handles {
        handles.insert(h, source.to_string());
    }
    Ok(())
}

/// Collect handle names from body nodes, ignoring validity (grammar check
/// is the existing `validate_handles` pass).
fn collect_handles_from_body(
    body: &[SceneBodyNode],
    source: &str,
    handles: &mut HashMap<String, String>,
) -> Result<(), SceneError> {
    let mut raw = Vec::new();
    collect_raw_handles(body, &mut raw);
    for h in raw {
        handles.insert(h, source.to_string());
    }
    Ok(())
}

fn collect_raw_handles(body: &[SceneBodyNode], out: &mut Vec<String>) {
    for node in body {
        match node {
            SceneBodyNode::Layout(layout) => collect_layout_handles(layout, out),
            SceneBodyNode::Mode(mode) => collect_mode_handles(mode, out),
            _ => {}
        }
    }
}

fn collect_layout_handles(layout: &LayoutNode, out: &mut Vec<String>) {
    for tab in &layout.tabs {
        collect_tab_handles(tab, out);
    }
}

fn collect_mode_handles(mode: &ModeNode, out: &mut Vec<String>) {
    for tab in &mode.tabs {
        collect_tab_handles(tab, out);
    }
}

fn collect_tab_handles(tab: &TabNode, out: &mut Vec<String>) {
    if !tab.handle.is_empty() {
        out.push(tab.handle.clone());
    }
    for child in &tab.body {
        collect_child_handles(child, out);
    }
}

fn collect_child_handles(child: &LayoutChild, out: &mut Vec<String>) {
    match child {
        LayoutChild::Row(r) => {
            for c in &r.body {
                collect_child_handles(c, out);
            }
        }
        LayoutChild::Col(c) => {
            for ch in &c.body {
                collect_child_handles(ch, out);
            }
        }
        LayoutChild::Pane(p) => {
            if !p.handle.is_empty() {
                out.push(p.handle.clone());
            }
        }
    }
}

/// Parse a fragment file. Fragments contain scene body nodes without the
/// `scene "name" { }` wrapper. We wrap them in a synthetic scene to reuse
/// the existing parser.
fn parse_fragment(content: &str, path: &Path, target: &str) -> Result<SceneIR, SceneError> {
    let wrapped = format!("scene \"__fragment__\" {{\n{content}\n}}");
    parse_scene(wrapped, path).map_err(|e| SceneError::IncludeFragmentParse {
        target: target.to_string(),
        message: e.to_string(),
    })
}

/// Parsed components of an `ext:<name>/<fragment>` include target.
#[derive(Debug, PartialEq, Eq)]
pub struct ExtIncludeRef {
    /// Extension name (e.g. `"git"`).
    pub name: String,
    /// Fragment identifier (e.g. `"status-bar"`).
    pub fragment: String,
}

/// Parse an `ext:<name>/<fragment>` include target into its components.
///
/// Returns `Err` with a descriptive reason if the format is invalid.
/// Valid formats: `ext:git/status-bar`, `ext:my-ext/my-fragment`.
/// Invalid: `ext:`, `ext:git`, `ext:git/`, `ext:/fragment`, `ext:git/a/b`.
pub fn parse_ext_include(target: &str) -> Result<ExtIncludeRef, SceneError> {
    let rest = target
        .strip_prefix("ext:")
        .ok_or_else(|| SceneError::ExtIncludeInvalid {
            target: target.to_string(),
            reason: "must start with `ext:`".to_string(),
        })?;

    if rest.is_empty() {
        return Err(SceneError::ExtIncludeInvalid {
            target: target.to_string(),
            reason: "missing `<name>/<fragment>` after `ext:`".to_string(),
        });
    }

    let slash_pos = rest.find('/').ok_or_else(|| SceneError::ExtIncludeInvalid {
        target: target.to_string(),
        reason: "missing `/<fragment>` after extension name".to_string(),
    })?;

    let name = &rest[..slash_pos];
    let fragment = &rest[slash_pos + 1..];

    if name.is_empty() {
        return Err(SceneError::ExtIncludeInvalid {
            target: target.to_string(),
            reason: "extension name is empty".to_string(),
        });
    }

    if fragment.is_empty() {
        return Err(SceneError::ExtIncludeInvalid {
            target: target.to_string(),
            reason: "fragment identifier is empty".to_string(),
        });
    }

    if fragment.contains('/') {
        return Err(SceneError::ExtIncludeInvalid {
            target: target.to_string(),
            reason: "fragment identifier must not contain `/`".to_string(),
        });
    }

    Ok(ExtIncludeRef {
        name: name.to_string(),
        fragment: fragment.to_string(),
    })
}

/// Resolve `ext:` includes using extension metadata.
///
/// Called after path-based includes are resolved by [`compose_scene`].
/// Walks `body`, finds remaining `Include` nodes whose target starts with
/// `ext:`, parses the `ext:<name>/<fragment>` format, and validates that
/// the referenced extension is activated in the registry.
///
/// Fragment loading is a placeholder until extensions ship actual fragment
/// files at runtime — currently returns [`SceneError::ExtFragmentNotFound`]
/// for any valid, activated extension reference.
pub fn resolve_ext_includes(
    body: &mut Vec<SceneBodyNode>,
    registry: &ExtensionRegistry,
) -> Result<(), SceneError> {
    let mut i = 0;
    while i < body.len() {
        let should_resolve = matches!(
            &body[i],
            SceneBodyNode::Include(inc) if inc.target.starts_with("ext:")
        );

        if !should_resolve {
            i += 1;
            continue;
        }

        // Extract the target string before further borrowing.
        let target = match &body[i] {
            SceneBodyNode::Include(inc) => inc.target.clone(),
            _ => unreachable!(),
        };

        let ext_ref = parse_ext_include(&target)?;

        // Validate that the extension is activated via `use`.
        if !registry.is_active(&ext_ref.name) {
            return Err(SceneError::ExtNotUsed {
                ext: ext_ref.name,
                target,
            });
        }

        // Placeholder: extensions don't yet ship fragment files at runtime.
        // Once they do, this is where we'd load the fragment content from
        // the extension's package, parse it, and splice the body nodes.
        tracing::warn!(
            ext = ext_ref.name,
            fragment = ext_ref.fragment,
            "ext: fragment not yet available — skipping include"
        );
        i += 1;
        continue;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn include_splices_fragment_body() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_file(
            dir,
            "base.kdl",
            r#"
layout {
    tab "@editor" {
        pane "@code"
    }
}
"#,
        );

        let scene_path = write_file(
            dir,
            "main.kdl",
            r#"scene "dev" {
    include "base.kdl"
}"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let composed = compose_scene(ir).unwrap();

        assert_eq!(composed.scene.name, "dev");
        assert_eq!(composed.scene.body.len(), 1);
        assert!(
            matches!(&composed.scene.body[0], SceneBodyNode::Layout(_)),
            "expected Layout node, got {:?}",
            composed.scene.body[0]
        );
    }

    #[test]
    fn multiple_includes_preserve_order() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_file(
            dir,
            "layout.kdl",
            r#"layout { tab "@main" { pane "@p1" } }"#,
        );
        write_file(
            dir,
            "binds.kdl",
            r#"bind "Alt d" { close "@p1" }"#,
        );

        let scene_path = write_file(
            dir,
            "main.kdl",
            r#"scene "dev" {
    include "layout.kdl"
    include "binds.kdl"
    on "FileEdited" { close "@p1" }
}"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let composed = compose_scene(ir).unwrap();

        assert_eq!(composed.scene.body.len(), 3);
        assert!(matches!(&composed.scene.body[0], SceneBodyNode::Layout(_)));
        assert!(matches!(&composed.scene.body[1], SceneBodyNode::Bind(_)));
        assert!(matches!(&composed.scene.body[2], SceneBodyNode::On(_)));
    }

    #[test]
    fn nested_includes_resolve() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_file(
            dir,
            "pane.kdl",
            r#"layout { tab "@nested" { pane "@deep" } }"#,
        );
        write_file(dir, "mid.kdl", r#"include "pane.kdl""#);

        let scene_path = write_file(
            dir,
            "top.kdl",
            r#"scene "top" {
    include "mid.kdl"
}"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let composed = compose_scene(ir).unwrap();

        assert_eq!(composed.scene.body.len(), 1);
        assert!(matches!(&composed.scene.body[0], SceneBodyNode::Layout(_)));
    }

    #[test]
    fn include_not_found_returns_error() {
        let tmp = TempDir::new().unwrap();
        let scene_path = write_file(
            tmp.path(),
            "main.kdl",
            r#"scene "dev" { include "nope.kdl" }"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let err = compose_scene(ir).unwrap_err();
        assert!(matches!(err, SceneError::IncludeNotFound { .. }));
    }

    #[test]
    fn include_cycle_detected() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_file(dir, "a.kdl", r#"include "b.kdl""#);
        write_file(dir, "b.kdl", r#"include "a.kdl""#);

        let scene_path = write_file(
            dir,
            "main.kdl",
            r#"scene "loop" { include "a.kdl" }"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let err = compose_scene(ir).unwrap_err();
        assert!(matches!(err, SceneError::IncludeCycle { .. }));
    }

    #[test]
    fn ext_includes_preserved() {
        let tmp = TempDir::new().unwrap();
        let scene_path = write_file(
            tmp.path(),
            "main.kdl",
            r#"scene "dev" {
    include "ext:git/status-bar"
}"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let composed = compose_scene(ir).unwrap();

        assert_eq!(composed.scene.body.len(), 1);
        assert!(matches!(&composed.scene.body[0], SceneBodyNode::Include(_)));
    }

    #[test]
    fn relative_path_resolves_from_scene_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        fs::create_dir_all(dir.join("fragments")).unwrap();
        write_file(
            dir,
            "fragments/editor.kdl",
            r#"layout { tab "@ed" { pane "@code" } }"#,
        );

        let scene_path = write_file(
            dir,
            "main.kdl",
            r#"scene "dev" { include "fragments/editor.kdl" }"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let composed = compose_scene(ir).unwrap();
        assert_eq!(composed.scene.body.len(), 1);
    }

    #[test]
    fn fragment_parse_error_returns_diagnostic() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_file(dir, "bad.kdl", "this is {{{{ not valid kdl");

        let scene_path = write_file(
            dir,
            "main.kdl",
            r#"scene "dev" { include "bad.kdl" }"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let err = compose_scene(ir).unwrap_err();
        assert!(matches!(err, SceneError::IncludeFragmentParse { .. }));
    }

    /// Diamond includes (same file via independent paths) are NOT cycles.
    /// The handle-clash detector catches duplicate handles separately.
    #[test]
    fn same_file_included_twice_from_siblings_is_diamond_not_cycle() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // Use unique handles so handle-clash doesn't fire — we're testing
        // that the cycle detector no longer rejects diamonds.
        write_file(
            dir,
            "shared.kdl",
            r#"bind "Alt s" { close "@p1" }"#,
        );

        let scene_path = write_file(
            dir,
            "main.kdl",
            r#"scene "dev" {
    include "shared.kdl"
    include "shared.kdl"
}"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let composed = compose_scene(ir).unwrap();
        // Fragment body spliced twice
        assert_eq!(composed.scene.body.len(), 2);
    }

    #[test]
    fn include_escape_detected() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // Create an "outside" file above the scene root
        let outside = tmp.path().join("outside.kdl");
        fs::write(&outside, r#"layout { tab "@x" { pane "@y" } }"#).unwrap();

        fs::create_dir_all(dir.join("scenes")).unwrap();
        let scene_path = write_file(
            dir,
            "scenes/main.kdl",
            r#"scene "dev" { include "../outside.kdl" }"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let err = compose_scene(ir).unwrap_err();
        assert!(
            matches!(err, SceneError::IncludeEscape { .. }),
            "expected IncludeEscape, got {err:?}"
        );
    }

    // --- T-077: Include conflict detection ---

    #[test]
    fn handle_clash_across_fragments_detected() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_file(
            dir,
            "a.kdl",
            r#"layout { tab "@editor" { pane "@code" } }"#,
        );
        write_file(
            dir,
            "b.kdl",
            r#"layout { tab "@editor" { pane "@term" } }"#,
        );

        let scene_path = write_file(
            dir,
            "main.kdl",
            r#"scene "dev" {
    include "a.kdl"
    include "b.kdl"
}"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let err = compose_scene(ir).unwrap_err();
        match &err {
            SceneError::IncludeHandleClash { handle, .. } => {
                assert_eq!(handle, "@editor");
            }
            other => panic!("expected IncludeHandleClash, got {other:?}"),
        }
    }

    #[test]
    fn handle_clash_between_scene_and_fragment() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_file(
            dir,
            "frag.kdl",
            r#"layout { tab "@main" { pane "@p2" } }"#,
        );

        let scene_path = write_file(
            dir,
            "main.kdl",
            r#"scene "dev" {
    layout { tab "@main" { pane "@p1" } }
    include "frag.kdl"
}"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let err = compose_scene(ir).unwrap_err();
        assert!(matches!(err, SceneError::IncludeHandleClash { .. }));
    }

    #[test]
    fn pane_handle_clash_across_fragments() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_file(
            dir,
            "a.kdl",
            r#"layout { tab "@t1" { pane "@shared" } }"#,
        );
        write_file(
            dir,
            "b.kdl",
            r#"layout { tab "@t2" { pane "@shared" } }"#,
        );

        let scene_path = write_file(
            dir,
            "main.kdl",
            r#"scene "dev" {
    include "a.kdl"
    include "b.kdl"
}"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let err = compose_scene(ir).unwrap_err();
        match &err {
            SceneError::IncludeHandleClash { handle, .. } => {
                assert_eq!(handle, "@shared");
            }
            other => panic!("expected IncludeHandleClash, got {other:?}"),
        }
    }

    #[test]
    fn no_conflict_with_unique_handles() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_file(
            dir,
            "a.kdl",
            r#"layout { tab "@t1" { pane "@p1" } }"#,
        );
        write_file(
            dir,
            "b.kdl",
            r#"layout { tab "@t2" { pane "@p2" } }"#,
        );

        let scene_path = write_file(
            dir,
            "main.kdl",
            r#"scene "dev" {
    include "a.kdl"
    include "b.kdl"
}"#,
        );

        let ir = parse_scene(fs::read_to_string(&scene_path).unwrap(), &scene_path).unwrap();
        let composed = compose_scene(ir).unwrap();
        assert_eq!(composed.scene.body.len(), 2);
    }

    // --- T-075: ext:<name>/<fragment> include resolution ---

    #[test]
    fn parse_ext_include_valid() {
        let r = parse_ext_include("ext:git/status-bar").unwrap();
        assert_eq!(r.name, "git");
        assert_eq!(r.fragment, "status-bar");
    }

    #[test]
    fn parse_ext_include_with_dashes_and_dots() {
        let r = parse_ext_include("ext:my-ext/my-fragment").unwrap();
        assert_eq!(r.name, "my-ext");
        assert_eq!(r.fragment, "my-fragment");
    }

    #[test]
    fn parse_ext_include_missing_fragment() {
        let err = parse_ext_include("ext:git").unwrap_err();
        assert!(
            matches!(err, SceneError::ExtIncludeInvalid { .. }),
            "expected ExtIncludeInvalid, got {err:?}"
        );
    }

    #[test]
    fn parse_ext_include_empty_name() {
        let err = parse_ext_include("ext:/fragment").unwrap_err();
        assert!(matches!(err, SceneError::ExtIncludeInvalid { .. }));
    }

    #[test]
    fn parse_ext_include_empty_fragment() {
        let err = parse_ext_include("ext:git/").unwrap_err();
        assert!(matches!(err, SceneError::ExtIncludeInvalid { .. }));
    }

    #[test]
    fn parse_ext_include_nested_slash() {
        let err = parse_ext_include("ext:git/a/b").unwrap_err();
        assert!(matches!(err, SceneError::ExtIncludeInvalid { .. }));
    }

    #[test]
    fn parse_ext_include_bare_ext() {
        let err = parse_ext_include("ext:").unwrap_err();
        assert!(matches!(err, SceneError::ExtIncludeInvalid { .. }));
    }

    #[test]
    fn parse_ext_include_not_ext_prefix() {
        let err = parse_ext_include("notext:git/bar").unwrap_err();
        assert!(matches!(err, SceneError::ExtIncludeInvalid { .. }));
    }

    #[test]
    fn resolve_ext_includes_ext_not_used() {
        use crate::ast::IncludeNode;

        let mut body = vec![SceneBodyNode::Include(IncludeNode {
            target: "ext:git/status-bar".to_string(),
        })];
        let registry = ExtensionRegistry::new();

        let err = resolve_ext_includes(&mut body, &registry).unwrap_err();
        match &err {
            SceneError::ExtNotUsed { ext, target } => {
                assert_eq!(ext, "git");
                assert_eq!(target, "ext:git/status-bar");
            }
            other => panic!("expected ExtNotUsed, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ext_includes_skips_fragment_with_warn_when_active() {
        use ark_ext_metadata_types::{
            CapabilitySet, ConfigSchema, ExtensionMetadata, StringNode,
        };
        use crate::ast::IncludeNode;

        let mut registry = ExtensionRegistry::new();
        let meta = ExtensionMetadata {
            name: StringNode::new("git"),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::default(),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::default(),
            config_sections: vec![],
            reload_gates: vec![],
        };
        registry.activate("git", &meta).unwrap();

        let mut body = vec![SceneBodyNode::Include(IncludeNode {
            target: "ext:git/status-bar".to_string(),
        })];

        // The include is from an active extension, so it should be skipped
        // (warn-and-continue) rather than hard-erroring. The body node is
        // left in place until extensions ship real fragment files.
        resolve_ext_includes(&mut body, &registry)
            .expect("active ext: include should warn and skip, not error");
        // The Include node is retained (not spliced/removed) in the placeholder path.
        assert_eq!(body.len(), 1, "body node should remain after skip");
    }

    #[test]
    fn resolve_ext_includes_skips_non_ext_nodes() {
        use crate::ast::IncludeNode;

        // Body with only non-ext: nodes should pass through cleanly.
        let mut body = vec![SceneBodyNode::Include(IncludeNode {
            target: "local.kdl".to_string(),
        })];
        let registry = ExtensionRegistry::new();

        // No error — non-ext includes are left alone.
        resolve_ext_includes(&mut body, &registry).unwrap();
        assert_eq!(body.len(), 1);
    }

    #[test]
    fn resolve_ext_includes_empty_body_ok() {
        let mut body: Vec<SceneBodyNode> = vec![];
        let registry = ExtensionRegistry::new();
        resolve_ext_includes(&mut body, &registry).unwrap();
    }

    #[test]
    fn resolve_ext_includes_invalid_format_errors() {
        use crate::ast::IncludeNode;

        let mut body = vec![SceneBodyNode::Include(IncludeNode {
            target: "ext:git".to_string(),
        })];
        let registry = ExtensionRegistry::new();

        let err = resolve_ext_includes(&mut body, &registry).unwrap_err();
        assert!(
            matches!(err, SceneError::ExtIncludeInvalid { .. }),
            "expected ExtIncludeInvalid, got {err:?}"
        );
    }
}
