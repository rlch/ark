//! T-027 completion gate (scene-2026-04-18) — integration tests for
//! stack round-trip and the Tier 5 stack ops.
//!
//! Three scenarios live here; miette-rendered goldens for
//! `view-type-mismatch`, `union-syntax-deferred`, and
//! `sizing-on-stack-child` live alongside the broader diagnostic
//! harness in `tests/errors.rs` (same `insta` pattern as every other
//! `scene_*` snapshot).
//!
//! Scenarios:
//!
//! - **(a) Stack round-trip** — parse → compile → layout-KDL emission
//!   end-to-end: a scene declaring `stack @s { pane "@c" { shell } }`
//!   surfaces in the rendered KDL as `pane stacked=true name="s"` with
//!   the child pane carrying its own `ARK_HANDLE=@c` env wrapper.
//! - **(b) `spawn_into @stack { foo attrs }` dispatch** — the Tier 5
//!   [`SpawnIntoOp`] records the expected `spawn_into_stack` mux call
//!   AND returns `@<stack>-<ulid>` per R-7 (child identity minted by
//!   ark, not by the caller).
//! - **(c) `clear @stack` dispatch** — the Tier 5 [`ClearOp`] records
//!   the expected `clear_stack` mux call.
//!
//! The dispatch tests use an inline `MuxHandle` test double so the
//! integration surface doesn't depend on the crate-private `MockMux`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kdl::{KdlDocument, KdlNode};

use ark_scene::ast::layout::Handle;
use ark_scene::compile::{compile_layout_kdl, compile_scene};
use ark_scene::id::SceneId;
use ark_scene::intent::{Intent, IntentContext, IntentValue, MuxHandle};
use ark_scene::ops::{spawn::SpawnIntoOp, stack::ClearOp};
use ark_scene::parse::parse_scene;
use ark_scene::rhai::Engine;
use ark_scene::view::ViewRegistry;
use ark_view::HandleId;

// ---------------------------------------------------------------------------
// (a) Stack round-trip — parse → compile → layout-KDL emission
// ---------------------------------------------------------------------------

#[test]
fn stack_round_trip_parse_compile_layout() {
    // T-027 (a): end-to-end — a scene with a `stack @s { pane "@c" {
    // shell } }` must parse, compile, and emit zellij-KDL carrying
    // the stack's `stacked=true` flag, `name="s"` identity, and the
    // child's `ARK_HANDLE=@c` env wrapper.
    let src = r#"
scene "dev" {
    layout {
        tab "@main" {
            stack "@subs" {
                pane "@seed" { shell }
            }
        }
    }
}
"#;
    let ir = parse_scene(src, "round_trip.kdl").expect("parse ok");
    // Pull the first layout block directly — mirrors the emitter's
    // public API in the existing `stack.rs` integration file.
    let layout = ir
        .scene
        .body
        .iter()
        .find_map(|n| match n {
            ark_scene::ast::SceneBodyNode::Layout(l) => Some(l),
            _ => None,
        })
        .expect("layout block present");
    let registry = ViewRegistry::with_primitives();
    let kdl = compile_layout_kdl(layout, &registry).expect("layout compile ok");
    let text = kdl.to_string();
    assert!(
        text.contains("stacked=#true") || text.contains("stacked=true"),
        "stack must emit stacked=true: {text}"
    );
    assert!(
        text.contains("\"subs\""),
        "stack identity must surface as name=\"subs\": {text}"
    );
    assert!(
        text.contains("ARK_HANDLE=@seed"),
        "declared child pane must carry ARK_HANDLE wrapper: {text}"
    );
    // Sanity — compile_scene (full pipeline) also accepts it.
    let engine = Engine::new();
    compile_scene(&engine, ir).expect("full scene compile ok");
}

// ---------------------------------------------------------------------------
// Test-scoped MuxHandle double
// ---------------------------------------------------------------------------

/// Minimal `MuxHandle` impl for integration tests. Records every call
/// into a `Vec<String>` and returns pinned child-ids for
/// `spawn_into_stack` so the `<stack>-<ulid>` contract is assertable.
#[derive(Debug, Default)]
struct TestMux {
    calls: Mutex<Vec<String>>,
    child_ulid: Mutex<Option<String>>,
    minted: Mutex<Vec<String>>,
}

impl TestMux {
    fn new() -> Self {
        Self::default()
    }
    fn pin_child_ulid(&self, ulid: &str) {
        *self.child_ulid.lock().unwrap() = Some(ulid.to_string());
    }
    fn take_calls(&self) -> Vec<String> {
        std::mem::take(&mut self.calls.lock().unwrap())
    }
    fn take_minted(&self) -> Vec<String> {
        std::mem::take(&mut self.minted.lock().unwrap())
    }
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl MuxHandle for TestMux {
    fn close_pane(&self, h: &Handle) -> Result<(), String> {
        self.record(format!("close_pane({})", h.raw()));
        Ok(())
    }
    fn close_tab(&self, h: &Handle) -> Result<(), String> {
        self.record(format!("close_tab({})", h.raw()));
        Ok(())
    }
    fn focus_pane(&self, h: &Handle) -> Result<(), String> {
        self.record(format!("focus_pane({})", h.raw()));
        Ok(())
    }
    fn focus_tab(&self, h: &Handle) -> Result<(), String> {
        self.record(format!("focus_tab({})", h.raw()));
        Ok(())
    }
    fn rename_tab(&self, h: &Handle, name: &str) -> Result<(), String> {
        self.record(format!("rename_tab({},{name})", h.raw()));
        Ok(())
    }
    fn resize_pane(&self, h: &Handle, d: &str, by: &str) -> Result<(), String> {
        self.record(format!("resize_pane({},{d},{by})", h.raw()));
        Ok(())
    }
    fn move_pane(&self, h: &Handle, to: &str) -> Result<(), String> {
        self.record(format!("move_pane({},{to})", h.raw()));
        Ok(())
    }
    fn pin_pane(&self, h: &Handle) -> Result<(), String> {
        self.record(format!("pin_pane({})", h.raw()));
        Ok(())
    }
    fn unpin_pane(&self, h: &Handle) -> Result<(), String> {
        self.record(format!("unpin_pane({})", h.raw()));
        Ok(())
    }
    fn handle_exists(&self, _h: &Handle) -> bool {
        false
    }
    fn spawn_pane(&self, h: &Handle, overlay: bool, view_body: Option<&str>) -> Result<(), String> {
        self.record(format!(
            "spawn_pane({},overlay={overlay},view={:?})",
            h.raw(),
            view_body
        ));
        Ok(())
    }
    fn new_tab(&self, h: &Handle, name: Option<&str>, cwd: Option<&str>) -> Result<(), String> {
        self.record(format!(
            "new_tab({},name={:?},cwd={:?})",
            h.raw(),
            name,
            cwd
        ));
        Ok(())
    }
    fn pipe(&self, f: &Handle, t: &Handle, payload: &str) -> Result<(), String> {
        self.record(format!("pipe({},{},{payload})", f.raw(), t.raw()));
        Ok(())
    }
    fn spawn_into_stack(
        &self,
        stack: &HandleId,
        view_body: Option<&str>,
    ) -> Result<HandleId, String> {
        self.record(format!(
            "spawn_into_stack({},view={:?})",
            stack.as_str(),
            view_body
        ));
        let ulid = self
            .child_ulid
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| ulid::Ulid::new().to_string().to_lowercase());
        let child = format!("{}-{ulid}", stack.as_str());
        self.minted.lock().unwrap().push(child.clone());
        Ok(HandleId::new(child))
    }
    fn clear_stack(&self, stack: &HandleId) -> Result<(), String> {
        self.record(format!("clear_stack({})", stack.as_str()));
        Ok(())
    }
}

fn ctx_with_mux(mux: Arc<TestMux>) -> IntentContext {
    let scene_id = SceneId::new(PathBuf::from("/tmp/scene.kdl"), b"scene \"t\" { }");
    IntentContext::new(scene_id, "scene").with_mux(mux)
}

fn node_from(src: &str) -> KdlNode {
    let doc: KdlDocument = src.parse().expect("test KDL parses");
    doc.nodes().first().expect("at least one node").clone()
}

// ---------------------------------------------------------------------------
// (b) spawn_into @stack { foo attrs } dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spawn_into_dispatches_to_mux_and_mints_child_ulid() {
    // T-027 (b): dispatch records the expected `spawn_into_stack(@subs,
    // view=Some("…"))` mux call AND returns `@subs-<ulid>` per R-7.
    let mux = Arc::new(TestMux::new());
    mux.pin_child_ulid("01jarkdemo000000000000000a");
    let ctx = ctx_with_mux(mux.clone());
    // Body carries a `foo attrs` inner view — the op preserves it and
    // passes it as the `view_body` argument to the mux.
    let node = node_from(r#"spawn_into "@subs" { command cmd="echo" }"#);
    let v = SpawnIntoOp
        .dispatch(&node, &ctx)
        .await
        .expect("dispatch ok");

    match v {
        IntentValue::String(child) => {
            // R-7 child identity: `<stack>-<lowercase-26-char-ulid>`.
            assert_eq!(
                child, "@subs-01jarkdemo000000000000000a",
                "child id must follow R-7 format"
            );
        }
        other => panic!("expected IntentValue::String, got {other:?}"),
    }
    let calls = mux.take_calls();
    assert_eq!(calls.len(), 1, "exactly one mux call: {calls:?}");
    assert!(
        calls[0].starts_with("spawn_into_stack(@subs,view="),
        "mux call must forward the stack handle + view body: {calls:?}"
    );
    assert!(
        calls[0].contains("command") && calls[0].contains("echo"),
        "view body must be serialised through to the mux: {calls:?}"
    );
    // Minted child list tracks the ULID-suffixed id.
    assert_eq!(
        mux.take_minted(),
        vec!["@subs-01jarkdemo000000000000000a".to_string()]
    );
}

// ---------------------------------------------------------------------------
// (c) clear @stack dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn clear_dispatches_to_mux() {
    // T-027 (c): dispatch records the expected `clear_stack(@subs)`
    // mux call. Idempotent per R-7 — a subsequent clear call records
    // a second call because the test-double always returns Ok, but
    // the op itself accepts that without surfacing an error.
    let mux = Arc::new(TestMux::new());
    let ctx = ctx_with_mux(mux.clone());
    let node = node_from(r#"clear "@subs""#);
    let v = ClearOp.dispatch(&node, &ctx).await.expect("dispatch ok");
    assert_eq!(v, IntentValue::None, "clear returns nothing on success");
    assert_eq!(
        mux.take_calls(),
        vec!["clear_stack(@subs)".to_string()],
        "mux must receive exactly one clear_stack call"
    );
}
