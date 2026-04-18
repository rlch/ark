//! Integration tests for layout lowering (T-034..T-040).
//!
//! Parses a scene KDL source, runs the compile + layout lowering
//! pipeline, and asserts the resulting zellij KDL has the expected
//! shape. Complements the unit tests in `src/compile/layout.rs`.

use std::collections::BTreeMap;

use ark_scene::ast::layout::{LayoutChild, PaneNode, TabNode, ViewRef};
use ark_scene::ast::{LayoutNode, SceneBodyNode};
use ark_scene::compile::layout::{compile_layout_kdl_with_ctx, SpawnContext};
use ark_scene::compile::{compile_layout_kdl, write_layout_artifact};
use ark_scene::parse::parse_scene;
use ark_scene::view::ViewRegistry;
use ark_scene::SceneId;

use kdl::{KdlDocument, KdlValue};

/// Build a layout directly from an AST — bypasses the scene parser so
/// these tests can assert shape without depending on T-026+ view
/// resolution being wired end-to-end.
fn layout_from_ast(tabs: Vec<TabNode>) -> LayoutNode {
    LayoutNode { tabs }
}

fn shell_pane(handle: &str) -> PaneNode {
    PaneNode {
        handle: format!("@{handle}"),
        span: None,
        cells: None,
        min: None,
        max: None,
        when: None,
        overlay: None,
        view: ViewRef {
            alias: "shell".to_string(),
            config_block: None,
        },
    }
}

fn sized_shell_pane(handle: &str, span: u32) -> PaneNode {
    PaneNode {
        span: Some(span),
        ..shell_pane(handle)
    }
}

fn tab_with(handle: &str, body: Vec<LayoutChild>) -> TabNode {
    TabNode {
        handle: format!("@{handle}"),
        cwd: None,
        name: None,
        focus: None,
        when: None,
        body,
    }
}

#[test]
fn compile_minimal_layout() {
    let layout = layout_from_ast(vec![tab_with(
        "main",
        vec![LayoutChild::Pane(shell_pane("m"))],
    )]);
    let doc = compile_layout_kdl(&layout, &ViewRegistry::with_primitives()).unwrap();
    let text = doc.to_string();
    KdlDocument::parse(&text).expect("output must re-parse");
    assert!(text.contains("layout"));
    assert!(text.contains("tab"));
    assert!(text.contains("ARK_HANDLE=@m"));
}

#[test]
fn row_split_direction_horizontal() {
    use ark_scene::ast::layout::RowNode;
    let layout = layout_from_ast(vec![tab_with(
        "t",
        vec![LayoutChild::Row(RowNode {
            body: vec![LayoutChild::Pane(shell_pane("p"))],
            when: None,
            span: None,
            cells: None,
            min: None,
            max: None,
        })],
    )]);
    let doc = compile_layout_kdl(&layout, &ViewRegistry::with_primitives()).unwrap();
    let text = doc.to_string();
    assert!(text.contains("split_direction"));
    assert!(text.contains("horizontal"));
}

#[test]
fn col_split_direction_vertical() {
    use ark_scene::ast::layout::ColNode;
    let layout = layout_from_ast(vec![tab_with(
        "t",
        vec![LayoutChild::Col(ColNode {
            body: vec![LayoutChild::Pane(shell_pane("p"))],
            when: None,
            span: None,
            cells: None,
            min: None,
            max: None,
        })],
    )]);
    let doc = compile_layout_kdl(&layout, &ViewRegistry::with_primitives()).unwrap();
    let text = doc.to_string();
    assert!(text.contains("vertical"));
}

#[test]
fn span_normalizes_to_percent() {
    use ark_scene::ast::layout::RowNode;
    let layout = layout_from_ast(vec![tab_with(
        "t",
        vec![LayoutChild::Row(RowNode {
            body: vec![
                LayoutChild::Pane(sized_shell_pane("a", 1)),
                LayoutChild::Pane(sized_shell_pane("b", 2)),
                LayoutChild::Pane(sized_shell_pane("c", 3)),
            ],
            when: None,
            span: None,
            cells: None,
            min: None,
            max: None,
        })],
    )]);
    let doc = compile_layout_kdl(&layout, &ViewRegistry::with_primitives()).unwrap();
    let text = doc.to_string();
    // 1/6 ≈ 16.7%, 2/6 ≈ 33.3%, 3/6 = 50%.
    assert!(text.contains("16.7%"), "text = {text}");
    assert!(text.contains("33.3%"), "text = {text}");
    assert!(text.contains("50%"), "text = {text}");
}

#[test]
fn cells_emits_raw_size() {
    let pane = PaneNode {
        cells: Some(60),
        ..shell_pane("p")
    };
    let layout = layout_from_ast(vec![tab_with("t", vec![LayoutChild::Pane(pane)])]);
    let doc = compile_layout_kdl(&layout, &ViewRegistry::with_primitives()).unwrap();
    let text = doc.to_string();
    // `size=60` (integer property, no percent).
    assert!(text.contains("size=60"));
}

#[test]
fn overlay_math_returns_expected_coords() {
    // Overlay parse hooks aren't wired through PaneNode yet (T-037
    // threading is parallel-safe but requires parser changes outside
    // Tier 4's scope). Exercise the math path directly through the
    // public helpers.
    use ark_scene::compile::layout::{
        anchor_overlay, parse_overlay_size, parse_pos, TerminalSize,
    };
    let pos = parse_pos("top-right").unwrap();
    let size = parse_overlay_size("20x10").unwrap();
    let (x, y, w, h) = anchor_overlay(pos, size, TerminalSize { cols: 80, rows: 24 });
    assert_eq!((x, y, w, h), (60, 0, 20, 10));
}

#[test]
fn ark_handle_env_wrapper_on_command_view() {
    // Two shell panes in a row should produce two distinct ARK_HANDLE
    // args so zellij's override-layout matcher (by command + args)
    // can disambiguate the subprocesses.
    use ark_scene::ast::layout::RowNode;
    let layout = layout_from_ast(vec![tab_with(
        "t",
        vec![LayoutChild::Row(RowNode {
            body: vec![
                LayoutChild::Pane(shell_pane("left")),
                LayoutChild::Pane(shell_pane("right")),
            ],
            when: None,
            span: None,
            cells: None,
            min: None,
            max: None,
        })],
    )]);
    let doc = compile_layout_kdl(&layout, &ViewRegistry::with_primitives()).unwrap();
    let text = doc.to_string();
    assert!(text.contains("ARK_HANDLE=@left"));
    assert!(text.contains("ARK_HANDLE=@right"));
}

#[test]
fn edit_primitive_has_no_env_wrapper() {
    let cfg = KdlDocument::parse_v2(r#"path "src/main.rs""#).unwrap();
    let pane = PaneNode {
        view: ViewRef {
            alias: "edit".to_string(),
            config_block: Some(cfg),
        },
        ..shell_pane("e")
    };
    let layout = layout_from_ast(vec![tab_with("t", vec![LayoutChild::Pane(pane)])]);
    let doc = compile_layout_kdl(&layout, &ViewRegistry::with_primitives()).unwrap();
    let text = doc.to_string();
    assert!(text.contains("edit="));
    assert!(
        !text.contains("ARK_HANDLE"),
        "edit panes must not have env wrapper: {text}"
    );
}

#[test]
fn write_layout_artifact_roundtrips_through_kdl_parser() {
    let layout = layout_from_ast(vec![tab_with(
        "main",
        vec![LayoutChild::Pane(shell_pane("m"))],
    )]);
    let doc = compile_layout_kdl(&layout, &ViewRegistry::with_primitives()).unwrap();
    let id = SceneId::new("/tmp/dev.kdl", b"content");

    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded in tests; env mutation acceptable.
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
    }

    let path = write_layout_artifact(&doc, &id).expect("write ok");
    assert!(path.exists());
    let text = std::fs::read_to_string(&path).unwrap();
    let reparsed = KdlDocument::parse(&text).expect("on-disk KDL must re-parse");
    assert!(!reparsed.nodes().is_empty());
}

#[test]
fn full_parse_and_compile_from_source() {
    // End-to-end: parse a scene, lift its layout subtree, lower it.
    // Note: pane view aliases come out empty because T-026+ view
    // resolution isn't wired through the parser yet; the compiler
    // falls back to `shell` for empty aliases so the end-to-end path
    // still produces valid zellij KDL.
    let src = r#"scene "dev" {
        layout {
            tab "@main" name="Main" {
                pane "@p"
            }
        }
    }"#;
    let ir = parse_scene(src, "dev.kdl").expect("parse");
    for node in &ir.scene.body {
        if let SceneBodyNode::Layout(l) = node {
            let doc =
                compile_layout_kdl(l, &ViewRegistry::with_primitives()).expect("compile");
            let text = doc.to_string();
            KdlDocument::parse(&text).expect("output must re-parse");
            assert!(text.contains("tab"));
            // Fallback alias populates ARK_HANDLE via the shell path.
            assert!(text.contains("ARK_HANDLE"));
            return;
        }
    }
    panic!("no layout node found in parsed scene");
}

#[test]
fn tab_property_emission_preserves_name() {
    let layout = layout_from_ast(vec![TabNode {
        handle: "@main".to_string(),
        cwd: None,
        name: Some("Dashboard".to_string()),
        focus: None,
        when: None,
        body: vec![LayoutChild::Pane(shell_pane("p"))],
    }]);
    let doc = compile_layout_kdl(&layout, &ViewRegistry::with_primitives()).unwrap();
    let text = doc.to_string();
    // name= may be quoted or bare; both acceptable.
    assert!(
        text.contains("name=Dashboard") || text.contains("name=\"Dashboard\""),
        "text = {text}"
    );
}

#[test]
fn integer_values_round_trip() {
    // Guard against KdlValue coercion regressions — sizing should emit
    // integers for cells.
    let pane = PaneNode {
        cells: Some(30),
        ..shell_pane("p")
    };
    let layout = layout_from_ast(vec![tab_with("t", vec![LayoutChild::Pane(pane)])]);
    let doc = compile_layout_kdl(&layout, &ViewRegistry::with_primitives()).unwrap();
    let text = doc.to_string();
    let parsed = KdlDocument::parse(&text).unwrap();
    // Walk and confirm at least one integer entry with value 30.
    let mut saw = false;
    walk_entries(&parsed, &mut |e| {
        if matches!(e, KdlValue::Integer(30)) {
            saw = true;
        }
    });
    assert!(saw, "expected integer 30 somewhere in: {text}");
}

fn walk_entries(doc: &KdlDocument, f: &mut dyn FnMut(&KdlValue)) {
    for node in doc.nodes() {
        for entry in node.entries() {
            f(entry.value());
        }
        if let Some(c) = node.children() {
            walk_entries(c, f);
        }
    }
}

// --------------------------------------------------------------------------
// Spawn-time `{Rhai}` brace-hole interpolation at layout-compile time
// --------------------------------------------------------------------------

#[test]
fn tab_cwd_rhai_hole_renders_to_spawn_cwd() {
    let layout = LayoutNode {
        tabs: vec![TabNode {
            handle: "@main".to_string(),
            cwd: Some("{cwd}".to_string()),
            name: None,
            focus: Some("true".to_string()),
            when: None,
            body: vec![LayoutChild::Pane(shell_pane("p"))],
        }],
    };

    let env: BTreeMap<String, String> = BTreeMap::new();
    let ctx = SpawnContext {
        cwd: "/real/working/dir",
        id: "abc",
        name: "demo",
        env: &env,
    };
    let doc = compile_layout_kdl_with_ctx(&layout, &ViewRegistry::with_primitives(), &ctx)
        .expect("compile with ctx");
    let rendered = doc.to_string();

    // Zellij KDL v1 serialises `/` as `\/` inside string literals.
    // Accept either rendering.
    let ok_slash = rendered.contains(r#"cwd="/real/working/dir""#)
        || rendered.contains(r#"cwd="\/real\/working\/dir""#);
    assert!(ok_slash, "expected resolved cwd; got: {rendered}");
    assert!(
        !rendered.contains("{cwd}"),
        "expected literal {{cwd}} to be gone; got: {rendered}"
    );
    // Re-parse guard — output must still be valid KDL (v1 + v2 parser).
    KdlDocument::parse(&rendered).expect("interp-rendered layout must re-parse");
}

#[test]
fn tab_cwd_literal_round_trips_unchanged() {
    // Literal (no brace-holes) cwd value must be emitted verbatim even
    // when a SpawnContext is supplied — the render path has a fast-path
    // for strings without `{`.
    let layout = LayoutNode {
        tabs: vec![TabNode {
            handle: "@main".to_string(),
            cwd: Some("/literal/path".to_string()),
            name: Some("LiteralName".to_string()),
            focus: None,
            when: None,
            body: vec![LayoutChild::Pane(shell_pane("p"))],
        }],
    };

    let env: BTreeMap<String, String> = BTreeMap::new();
    let ctx = SpawnContext {
        cwd: "/unused",
        id: "unused-id",
        name: "unused-name",
        env: &env,
    };
    let doc = compile_layout_kdl_with_ctx(&layout, &ViewRegistry::with_primitives(), &ctx)
        .expect("literal cwd must compile");
    let rendered = doc.to_string();
    let ok_cwd = rendered.contains(r#"cwd="/literal/path""#)
        || rendered.contains(r#"cwd="\/literal\/path""#);
    assert!(ok_cwd, "expected literal cwd preserved; got: {rendered}");
    assert!(
        rendered.contains("name=LiteralName") || rendered.contains(r#"name="LiteralName""#),
        "expected literal name preserved; got: {rendered}"
    );
}

#[test]
fn tab_name_rhai_hole_renders_to_session_name() {
    let layout = LayoutNode {
        tabs: vec![TabNode {
            handle: "@main".to_string(),
            cwd: None,
            name: Some("{name}-main".to_string()),
            focus: None,
            when: None,
            body: vec![LayoutChild::Pane(shell_pane("p"))],
        }],
    };

    let env: BTreeMap<String, String> = BTreeMap::new();
    let ctx = SpawnContext {
        cwd: "/cwd",
        id: "id123",
        name: "demo",
        env: &env,
    };
    let doc = compile_layout_kdl_with_ctx(&layout, &ViewRegistry::with_primitives(), &ctx)
        .expect("compile with ctx");
    let rendered = doc.to_string();
    assert!(
        rendered.contains(r#"name="demo-main""#),
        "expected resolved tab name `demo-main`; got: {rendered}"
    );
    assert!(
        !rendered.contains("{name}"),
        "expected literal {{name}} to be gone; got: {rendered}"
    );
}
