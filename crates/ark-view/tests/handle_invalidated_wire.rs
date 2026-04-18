//! T-015 (build-site-soul-phase-2.md): wire-shape golden for the
//! `ark.handle.invalidated` ExtEvent payload.
//!
//! The event broadcasts every handle termination (per cavekit-
//! soul-phase-2-ark-view.md R7). This test pins the exact JSON
//! payload shape so extensions can rely on it — `{ handle, cause }`
//! with handle as opaque string (R5) and cause as snake_case tag
//! (T-007).
//!
//! If this test fails, a type upstream changed its serde representation
//! and every extension's event router breaks. Update this test ONLY
//! alongside a documented wire-contract change + MINOR-version bump
//! per decision #4c.

use ark_view::{HandleId, InvalidationCause};
use serde_json::json;

/// Strongly-typed payload for the event — pins the field names.
#[derive(serde::Serialize, serde::Deserialize)]
struct InvalidatedPayload {
    handle: HandleId,
    cause: InvalidationCause,
}

#[test]
fn user_closed_payload_matches_golden() {
    let p = InvalidatedPayload {
        handle: HandleId::new("abc-123"),
        cause: InvalidationCause::UserClosed,
    };
    let json_val = serde_json::to_value(&p).unwrap();
    let golden = json!({
        "handle": "abc-123",
        "cause": "user_closed",
    });
    assert_eq!(json_val, golden, "user_closed payload drift: {json_val}");
}

#[test]
fn scene_reload_dropped_payload_matches_golden() {
    let p = InvalidatedPayload {
        handle: HandleId::new("xyz-789"),
        cause: InvalidationCause::SceneReloadDropped,
    };
    assert_eq!(
        serde_json::to_value(&p).unwrap(),
        json!({ "handle": "xyz-789", "cause": "scene_reload_dropped" }),
    );
}

#[test]
fn session_ended_payload_matches_golden() {
    let p = InvalidatedPayload {
        handle: HandleId::new("ses-1"),
        cause: InvalidationCause::SessionEnded,
    };
    assert_eq!(
        serde_json::to_value(&p).unwrap(),
        json!({ "handle": "ses-1", "cause": "session_ended" }),
    );
}

#[test]
fn handle_is_scalar_not_object() {
    let p = InvalidatedPayload {
        handle: HandleId::new("h"),
        cause: InvalidationCause::UserClosed,
    };
    let v = serde_json::to_value(&p).unwrap();
    assert!(
        v["handle"].is_string(),
        "handle must serialise as plain string, got {:?}",
        v["handle"]
    );
    assert!(
        v["cause"].is_string(),
        "cause must serialise as plain tag string, got {:?}",
        v["cause"]
    );
}

#[test]
fn payload_roundtrips_via_serde_value() {
    let p = InvalidatedPayload {
        handle: HandleId::new("round-trip"),
        cause: InvalidationCause::UserClosed,
    };
    let val = serde_json::to_value(&p).unwrap();
    let back: InvalidatedPayload = serde_json::from_value(val).unwrap();
    assert_eq!(back.handle.as_str(), "round-trip");
    matches!(back.cause, InvalidationCause::UserClosed);
}

/// Full ExtEvent-envelope shape: when this payload rides inside
/// `CoreEvent::Ext(ExtEvent { ext: "ark", kind: "handle.invalidated", ... })`,
/// the full JSON matches the documented wire shape.
///
/// This test is documentary — we construct the envelope by hand since
/// ark-view depends on `serde_json` only, not `ark-types`.
#[test]
fn ark_handle_invalidated_envelope_shape_documented() {
    let payload = serde_json::to_value(&InvalidatedPayload {
        handle: HandleId::new("h1"),
        cause: InvalidationCause::UserClosed,
    })
    .unwrap();
    let envelope = json!({
        "ext": "ark",
        "kind": "handle.invalidated",
        "payload": payload,
    });
    assert_eq!(envelope["ext"], "ark");
    assert_eq!(envelope["kind"], "handle.invalidated");
    assert_eq!(envelope["payload"]["handle"], "h1");
    assert_eq!(envelope["payload"]["cause"], "user_closed");
}
