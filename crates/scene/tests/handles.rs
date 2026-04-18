//! Handle validation tests (T-014).
//!
//! Exercises `validate_handles` from `ark_scene::validate::handles`.

use ark_scene::parse::parse_scene;
use ark_scene::validate::handles::validate_handles;

/// Scene with unique, well-formed handles passes with zero errors.
#[test]
fn valid_handles_pass() {
    let src = r#"
scene "dev" {
    layout {
        tab "@main" focus="true" {
            row {
                pane "@editor" span=2
                pane "@shell"
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "valid.kdl").expect("well-formed scene should parse");
    let errors = validate_handles(&ir);
    assert!(
        errors.is_empty(),
        "expected no handle errors, got: {errors:?}"
    );
}

/// Two panes sharing `@shell` produce a HandleClash error.
#[test]
fn duplicate_handle_detected() {
    let src = r#"
scene "dup" {
    layout {
        tab "@main" {
            pane "@shell"
            pane "@shell"
        }
    }
}
"#;
    let ir = parse_scene(src, "dup.kdl").expect("scene should parse");
    let errors = validate_handles(&ir);
    assert!(
        !errors.is_empty(),
        "expected at least one error for duplicate handles"
    );

    let has_clash = errors.iter().any(
        |e| matches!(e, ark_scene::SceneError::HandleClash { handle, .. } if handle == "@shell"),
    );
    assert!(
        has_clash,
        "expected HandleClash for @shell, got: {errors:?}"
    );
}

/// A tab with handle `"main"` (no `@` prefix) is rejected.
#[test]
fn handle_missing_at_prefix_rejected() {
    let src = r#"
scene "bad" {
    layout {
        tab "main" {
            pane "@ok"
        }
    }
}
"#;
    let ir = parse_scene(src, "bad.kdl").expect("scene should parse");
    let errors = validate_handles(&ir);
    assert!(
        !errors.is_empty(),
        "expected error for handle without @ prefix"
    );

    let has_missing = errors
        .iter()
        .any(|e| matches!(e, ark_scene::SceneError::HandleMissing { node: "tab", .. }));
    assert!(
        has_missing,
        "expected HandleMissing for tab, got: {errors:?}"
    );
}

/// A handle `@editor` declared in both layout and mode blocks clashes.
#[test]
fn handle_in_mode_conflicts_with_layout() {
    let src = r#"
scene "conflict" {
    layout {
        tab "@main" {
            pane "@editor"
        }
    }
    mode "review" {
        tab "@review" {
            pane "@editor"
        }
    }
}
"#;
    let ir = parse_scene(src, "conflict.kdl").expect("scene should parse");
    let errors = validate_handles(&ir);
    assert!(
        !errors.is_empty(),
        "expected HandleClash across layout + mode"
    );

    let has_clash = errors.iter().any(
        |e| matches!(e, ark_scene::SceneError::HandleClash { handle, .. } if handle == "@editor"),
    );
    assert!(
        has_clash,
        "expected HandleClash for @editor, got: {errors:?}"
    );
}

/// A pane with an empty handle string is rejected.
#[test]
fn empty_handle_rejected() {
    let src = r#"
scene "empty" {
    layout {
        tab "@main" {
            pane ""
        }
    }
}
"#;
    let ir = parse_scene(src, "empty.kdl").expect("scene should parse");
    let errors = validate_handles(&ir);
    assert!(!errors.is_empty(), "expected error for empty handle");

    let has_missing = errors
        .iter()
        .any(|e| matches!(e, ark_scene::SceneError::HandleMissing { node: "pane", .. }));
    assert!(
        has_missing,
        "expected HandleMissing for pane, got: {errors:?}"
    );
}
