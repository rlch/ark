//! T-032 + T-033 (claude-code-ext R5b) — `scene_compile_hook` raw-cmd
//! fallback regression suite.
//!
//! Covers:
//!
//! * **T-032 happy path**: a scene with `command cmd="claude"` plus
//!   `match_cmds = ["claude"]` config → response carries one
//!   `EnvInjection` for that pane, merging `CLAUDE_HOOK_SOCKET=<path>`
//!   into the pane env.
//! * **T-032 miss**: a scene with `command cmd="shell"` plus
//!   `match_cmds = ["claude"]` config → response carries zero
//!   injections (cmd doesn't match).
//! * **T-032 default-off**: empty `match_cmds` (the default) produces
//!   zero injections regardless of scene content. R5b is opt-in.
//! * **T-033 scope bound**: raw-cmd fallback provides NO typed subagent
//!   fan-out. Only env injection is contributed; the response carries
//!   no `spawn_stack` / `subagent_*` surface. This is the regression
//!   that keeps the fallback narrow per kit R5b: "no view struct, no
//!   typed attrs".
//! * **Fault tolerance**: malformed `partial_scene` JSON does not fail
//!   the compile hook — returns an empty-injections response so scene
//!   compile continues uninterrupted.

use ark_ext_claude_code::{ClaudeCodeExtension, EnvInjection, SceneCompileContributions};
use ark_ext_proto::{ArkExtension, SceneCompileHookRequest};

/// Decode the opaque `contributions` JSON back into
/// [`SceneCompileContributions`] for structural assertions.
fn decode_contributions(json: &str) -> SceneCompileContributions {
    serde_json::from_str(json).expect("contributions should always decode")
}

fn sample_scene_json(panes: &[(&str, &str, &str)]) -> String {
    // Shape: { "panes": [ { "id", "view": { "kind", "cmd" } } ] }
    let arr: Vec<_> = panes
        .iter()
        .map(|(id, kind, cmd)| {
            serde_json::json!({
                "id": id,
                "view": { "kind": kind, "cmd": cmd },
            })
        })
        .collect();
    serde_json::json!({ "panes": arr }).to_string()
}

#[tokio::test]
async fn t032_match_cmds_injects_cc_hook_socket_into_matched_pane() {
    let ext = ClaudeCodeExtension::new().with_match_cmds(vec!["claude".to_string()]);
    let scene = sample_scene_json(&[("chat", "command", "claude")]);
    let resp = ext
        .scene_compile_hook(SceneCompileHookRequest {
            partial_scene: scene,
        })
        .await
        .expect("scene_compile_hook must never fail for valid input");

    let contribs = decode_contributions(&resp.contributions);
    assert_eq!(
        contribs.env_injections.len(),
        1,
        "exactly one injection expected for the matched `claude` pane"
    );
    let inj = &contribs.env_injections[0];
    assert_eq!(inj.pane_id, "chat");
    assert!(
        inj.env.contains_key("CLAUDE_HOOK_SOCKET"),
        "CLAUDE_HOOK_SOCKET must be the env key injected per R5b; got {:?}",
        inj.env
    );
    let sock = inj.env.get("CLAUDE_HOOK_SOCKET").unwrap();
    assert!(
        sock.ends_with("cc-hook.sock"),
        "socket path should point at the cc-hook socket file; got `{sock}`"
    );
}

#[tokio::test]
async fn t032_cmd_mismatch_produces_zero_injections() {
    // `match_cmds = ["claude"]` does NOT match a `cmd="shell"` pane.
    let ext = ClaudeCodeExtension::new().with_match_cmds(vec!["claude".to_string()]);
    let scene = sample_scene_json(&[("term", "command", "shell")]);
    let resp = ext
        .scene_compile_hook(SceneCompileHookRequest {
            partial_scene: scene,
        })
        .await
        .expect("scene_compile_hook must never fail");
    let contribs = decode_contributions(&resp.contributions);
    assert!(
        contribs.env_injections.is_empty(),
        "no injections expected when cmd doesn't match; got {:?}",
        contribs.env_injections
    );
}

#[tokio::test]
async fn t032_default_empty_match_cmds_is_a_no_op() {
    // R5b is opt-in. Default construction yields an empty match list.
    let ext = ClaudeCodeExtension::new();
    assert!(
        ext.match_cmds().is_empty(),
        "default match_cmds must be empty (R5b is opt-in)"
    );
    let scene = sample_scene_json(&[("chat", "command", "claude")]);
    let resp = ext
        .scene_compile_hook(SceneCompileHookRequest {
            partial_scene: scene,
        })
        .await
        .expect("scene_compile_hook must never fail");
    let contribs = decode_contributions(&resp.contributions);
    assert!(
        contribs.env_injections.is_empty(),
        "no injections expected when match_cmds is empty; got {:?}",
        contribs.env_injections
    );
}

#[tokio::test]
async fn t033_raw_cmd_fallback_does_not_emit_typed_subagent_contributions() {
    // T-033 regression: the raw-cmd fallback MUST NOT produce any
    // typed-subagent surface (no spawn-stack, no view-binding). The
    // response shape is strictly `{ env_injections }`; anything else
    // would represent type leakage from R5 (typed view) into R5b (raw
    // fallback).
    let ext = ClaudeCodeExtension::new().with_match_cmds(vec!["claude".to_string()]);
    let scene = sample_scene_json(&[("chat", "command", "claude")]);
    let resp = ext
        .scene_compile_hook(SceneCompileHookRequest {
            partial_scene: scene,
        })
        .await
        .expect("scene_compile_hook must never fail");

    // Parse as raw serde_json::Value first so we can assert on the
    // object keys — if a future edit adds e.g. `spawn_stack`, this
    // test fails loud before review catches it.
    let raw: serde_json::Value =
        serde_json::from_str(&resp.contributions).expect("contributions must be valid JSON");
    let obj = raw
        .as_object()
        .expect("contributions must be a JSON object");
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(|k| k.as_str()).collect();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from(["env_injections"]),
        "R5b fallback must ONLY contribute env_injections (no typed subagent fan-out); got keys {:?}",
        keys
    );

    // Structural check: the one injection contributes env ONLY, no
    // subagent spawn attrs.
    let contribs = decode_contributions(&resp.contributions);
    let inj: &EnvInjection = &contribs.env_injections[0];
    // BTreeMap doesn't carry extra keys — but assert on the exact env
    // set so we catch future drift that might smuggle a stack-handle
    // id through.
    let env_keys: std::collections::BTreeSet<&str> = inj.env.keys().map(|k| k.as_str()).collect();
    assert_eq!(
        env_keys,
        std::collections::BTreeSet::from(["CLAUDE_HOOK_SOCKET"]),
        "R5b env injection must contain exactly CLAUDE_HOOK_SOCKET; got keys {:?}",
        env_keys
    );
}

#[tokio::test]
async fn scene_compile_hook_is_fault_tolerant_to_malformed_json() {
    // R5b must never block scene compile. Garbage input → empty
    // injections, Ok response.
    let ext = ClaudeCodeExtension::new().with_match_cmds(vec!["claude".to_string()]);
    let resp = ext
        .scene_compile_hook(SceneCompileHookRequest {
            partial_scene: "this is not json {".to_string(),
        })
        .await
        .expect("malformed partial_scene must NOT produce an Err");
    let contribs = decode_contributions(&resp.contributions);
    assert!(
        contribs.env_injections.is_empty(),
        "malformed partial_scene → no injections"
    );
}

#[tokio::test]
async fn scene_compile_hook_injects_all_matching_panes_but_only_those() {
    // Mixed scene: 3 panes, one `claude`, one `shell`, one non-command.
    // `match_cmds` carries two entries — the extra one doesn't appear
    // in the scene. Expected: exactly one injection for the `claude`
    // pane, zero for `shell`, zero for the non-command.
    let ext = ClaudeCodeExtension::new()
        .with_match_cmds(vec!["claude".to_string(), "anthropic".to_string()]);
    let scene = serde_json::json!({
        "panes": [
            { "id": "chat", "view": { "kind": "command", "cmd": "claude" } },
            { "id": "term", "view": { "kind": "command", "cmd": "shell" } },
            { "id": "editor", "view": { "kind": "editor", "cmd": "nvim" } },
        ]
    })
    .to_string();
    let resp = ext
        .scene_compile_hook(SceneCompileHookRequest {
            partial_scene: scene,
        })
        .await
        .expect("scene_compile_hook must never fail");
    let contribs = decode_contributions(&resp.contributions);
    assert_eq!(
        contribs.env_injections.len(),
        1,
        "only the `claude` pane should match; got {:?}",
        contribs.env_injections
    );
    assert_eq!(contribs.env_injections[0].pane_id, "chat");
}
