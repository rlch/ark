//! Snapshot-test harness for `SceneError` miette-rendered diagnostics (T-010).
//!
//! Regenerate snapshots via `cargo insta review` after intentional
//! diagnostic-text changes; snapshots serve as the `ark scene check`
//! diagnostic contract per R12.

use ark_scene::error::SceneError;
use miette::{GraphicalReportHandler, GraphicalTheme, NamedSource};

const FAKE_KDL: &str = "scene \"x\" { bad }";
const BAD_OFFSET: usize = 12;
const BAD_LEN: usize = 3;

fn src() -> NamedSource<String> {
    NamedSource::new("scene.kdl", FAKE_KDL.to_string())
}

fn render(err: &SceneError) -> String {
    let mut out = String::new();
    GraphicalReportHandler::new()
        .with_theme(GraphicalTheme::unicode_nocolor())
        .render_report(&mut out, err)
        .expect("miette render should succeed for SceneError");
    out
}

fn snap(slug: &str, err: &SceneError) {
    insta::assert_snapshot!(slug, render(err));
}

#[test]
fn scene_parse() {
    snap(
        "scene_parse",
        &SceneError::Parse {
            message: "unexpected token".into(),
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_misplaced_node() {
    snap(
        "scene_misplaced-node",
        &SceneError::MisplacedNode {
            node: "bad".into(),
            parent: "scene".into(),
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_unknown_node() {
    snap(
        "scene_unknown-node",
        &SceneError::UnknownNode {
            node: "bad".into(),
            help: "did you mean `bind`? Scene-root admits: `use`, `include`, `layout`, `mode`, `on`, `bind`, `clear-reactions`, `clear-bind`, `disable-extension`.".into(),
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_unknown_view() {
    snap(
        "scene_unknown-view",
        &SceneError::UnknownView {
            view: "bad".into(),
            help: "did you mean `shell`? Available views: `command`, `shell`, `edit`.".into(),
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_handle_clash() {
    snap(
        "scene_handle-clash",
        &SceneError::HandleClash {
            handle: "@bad".into(),
            src: src(),
            first: (BAD_OFFSET, BAD_LEN).into(),
            second: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_handle_type_mismatch() {
    snap(
        "scene_handle-type-mismatch",
        &SceneError::HandleTypeMismatch {
            op: "rename".into(),
            handle: "@bad".into(),
            expected: "tab",
            actual: "pane",
            src: src(),
            use_span: (BAD_OFFSET, BAD_LEN).into(),
            decl_span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_handle_missing() {
    snap(
        "scene_handle-missing",
        &SceneError::HandleMissing {
            node: "pane",
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_unknown_event_field() {
    snap(
        "scene_unknown-event-field",
        &SceneError::UnknownEventField {
            event_kind: "AgentTurn".into(),
            field: "bad".into(),
            help: "did you mean `phase`? Available fields: `phase`, `session_id`.".into(),
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_op_failed() {
    snap(
        "scene_op-failed",
        &SceneError::OpFailed {
            op: "spawn".into(),
            message: "no such handle `@missing`".into(),
        },
    );
}

#[test]
fn scene_unknown_op() {
    snap(
        "scene_unknown-op",
        &SceneError::UnknownOp {
            op: "bad".into(),
            help: "did you mean `focus`? Available ops: `focus`, `close`, `rename`, ...".into(),
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_ambiguous_file_shape() {
    snap(
        "scene_ambiguous-file-shape",
        &SceneError::AmbiguousFileShape {
            src: src(),
            scene_span: (BAD_OFFSET, BAD_LEN).into(),
            layout_span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_empty_or_unknown() {
    snap(
        "scene_empty-or-unknown",
        &SceneError::EmptyOrUnknown {
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_engine_conflict() {
    snap(
        "scene_engine-conflict",
        &SceneError::EngineConflict {
            use_name: "claude-code".into(),
            src: src(),
            inline_span: (BAD_OFFSET, BAD_LEN).into(),
            use_span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_rhai_parse() {
    snap(
        "scene_rhai-parse",
        &SceneError::RhaiParse {
            message: "unexpected token `+`".into(),
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_rhai_scope_mismatch() {
    snap(
        "scene_rhai-scope-mismatch",
        &SceneError::RhaiScopeMismatch {
            message: "`event` unavailable in spawn scope".into(),
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn scene_rhai_eval() {
    snap(
        "scene_rhai-eval",
        &SceneError::RhaiEval {
            message: "nil access on `agent.missing_field`".into(),
        },
    );
}

#[test]
fn scene_rhai_oom() {
    snap("scene_rhai-oom", &SceneError::RhaiOom { limit: 10_000 });
}

#[test]
fn ext_missing() {
    snap(
        "ext_missing",
        &SceneError::ExtMissing {
            name: "bad".into(),
            help: "did you mean `status`? Searched: compiled-in, ~/.local/share/ark/extensions/, .ark/extensions/.".into(),
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn ext_cycle() {
    snap(
        "ext_cycle",
        &SceneError::ExtCycle {
            trail: vec![
                "alpha".into(),
                "beta".into(),
                "gamma".into(),
                "alpha".into(),
            ],
        },
    );
}

#[test]
fn ext_crashed() {
    snap(
        "ext_crashed",
        &SceneError::ExtCrashed {
            name: "status".into(),
            reason: "exit code 137 (SIGKILL)".into(),
        },
    );
}

#[test]
fn ext_reserved_namespace() {
    snap(
        "ext_reserved-namespace",
        &SceneError::ExtReservedNamespace {
            ext: "core".into(),
            attempted: "ark.core.frobnicate".into(),
        },
    );
}

#[test]
fn ext_bad_config() {
    snap(
        "ext_bad-config",
        &SceneError::ExtBadConfig {
            ext: "status".into(),
            message: "expected `int`, got `string`".into(),
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn ext_proto_unsupported_version() {
    snap(
        "ext-proto_unsupported-version",
        &SceneError::ExtProtoUnsupportedVersion {
            ext: "status".into(),
            required: ">= 2.0".into(),
            actual: "1.0".into(),
        },
    );
}

#[test]
fn ext_proto_capability_denied() {
    snap(
        "ext-proto_capability-denied",
        &SceneError::ExtProtoCapabilityDenied {
            ext: "status".into(),
            capability: "fs.write".into(),
        },
    );
}

#[test]
fn op_unresolved_ref() {
    snap(
        "op_unresolved-ref",
        &SceneError::OpUnresolvedRef {
            op: "focus".into(),
            kind: "handle".into(),
            name: "@bad".into(),
            help: "did you mean `@main`? Available handles: `@main`, `@side`.".into(),
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}

#[test]
fn op_handle_type_mismatch() {
    snap(
        "op_handle-type-mismatch",
        &SceneError::OpHandleTypeMismatch {
            op: "resize".into(),
            arg: "target".into(),
            handle: "@bad".into(),
            expected: "pane",
            actual: "tab",
            src: src(),
            span: (BAD_OFFSET, BAD_LEN).into(),
        },
    );
}
