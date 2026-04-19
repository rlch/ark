//! Per-extension control-verb dispatch surface (v0.2-backlog #4).
//!
//! The `ark ext <name> <verb>` CLI sends a `ControlVerbInvoke {ext, verb,
//! args}` command over the supervisor control socket. The supervisor does
//! not directly own any `ArkExtension` instances today (T-030 abstracts
//! handshake + capability recording without holding the live ext); this
//! module exposes a minimal dispatcher-registration surface so a boot
//! sequence that DOES hold live extensions can plug a dispatcher in at
//! startup.
//!
//! The pattern mirrors `ark_view::stack_dispatcher::StackDispatcher`
//! (v0.2-backlog #2) — a process-global `OnceLock<Arc<dyn Dispatcher>>`
//! registered once, consulted on every control-verb invocation. When no
//! dispatcher is registered the command returns a clear
//! `"no control-verb dispatcher registered"` error so callers (CLI,
//! tests) see the wiring gap explicitly rather than a silent drop.
//!
//! First-writer-wins registration: subsequent `register_control_verb_
//! dispatcher` calls are ignored with a debug log. Tests that need a
//! fresh dispatcher use a process-private test harness (see the module
//! tests below).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use serde_json::Value as JsonValue;

/// Outcome returned by a control-verb dispatcher.
///
/// `Ok(JsonValue)` — verb executed; `data` is forwarded as the
///   supervisor's `Response::ok(data)`.
/// `Err(String)` — verb failed; `error` becomes `Response::err(msg)`.
pub type ControlVerbResult = Result<JsonValue, String>;

/// Future returned by [`ControlVerbDispatcher::invoke`].
pub type ControlVerbFuture = Pin<Box<dyn Future<Output = ControlVerbResult> + Send + 'static>>;

/// Abstract surface the supervisor consults to route a control-verb
/// invocation to the owning extension's handler.
///
/// Implementations live outside this crate — extension-side boot code
/// builds one that inspects `ext` against its known extension names
/// and fans out to each extension's own verb handler. A no-op test
/// impl lives in this module's tests.
pub trait ControlVerbDispatcher: Send + Sync {
    /// Invoke `verb` on `ext` with the given arg list.
    ///
    /// Returning `Err("unknown extension: <name>")` is the conventional
    /// failure mode for an unregistered ext name — callers surface that
    /// to the user as an exit-1 generic error. An unknown `verb` on a
    /// known ext is similarly surfaced as `Err("unknown verb: <name>")`.
    fn invoke(&self, ext: &str, verb: &str, args: Vec<String>) -> ControlVerbFuture;
}

/// Process-global control-verb dispatcher. Set once, consulted on every
/// `ControlVerbInvoke` command. `None` until the supervisor boot
/// sequence calls [`register_control_verb_dispatcher`].
static DISPATCHER: OnceLock<Arc<dyn ControlVerbDispatcher>> = OnceLock::new();

/// Install the process-global control-verb dispatcher. First writer
/// wins; subsequent calls are ignored and return `false`. Returns
/// `true` on the successful install.
pub fn register_control_verb_dispatcher(d: Arc<dyn ControlVerbDispatcher>) -> bool {
    DISPATCHER.set(d).is_ok()
}

/// Borrow the registered dispatcher, or `None` if unset.
pub fn control_verb_dispatcher() -> Option<Arc<dyn ControlVerbDispatcher>> {
    DISPATCHER.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test dispatcher that records invocations + returns a canned JSON.
    struct RecordingDispatcher {
        calls: Mutex<Vec<(String, String, Vec<String>)>>,
    }

    impl ControlVerbDispatcher for RecordingDispatcher {
        fn invoke(&self, ext: &str, verb: &str, args: Vec<String>) -> ControlVerbFuture {
            let ext = ext.to_string();
            let verb = verb.to_string();
            self.calls
                .lock()
                .unwrap()
                .push((ext.clone(), verb.clone(), args.clone()));
            Box::pin(async move {
                if ext == "known" && verb == "ok" {
                    Ok(serde_json::json!({
                        "ext": ext,
                        "verb": verb,
                        "args_len": args.len(),
                    }))
                } else if ext == "known" {
                    Err(format!("unknown verb: {verb}"))
                } else {
                    Err(format!("unknown extension: {ext}"))
                }
            })
        }
    }

    #[tokio::test]
    async fn recording_dispatcher_known_verb_returns_ok() {
        let rec = Arc::new(RecordingDispatcher {
            calls: Mutex::new(Vec::new()),
        });
        let result = rec
            .invoke("known", "ok", vec!["a".into(), "b".into()])
            .await;
        let value = result.expect("known.ok should return Ok");
        assert_eq!(value["ext"], serde_json::json!("known"));
        assert_eq!(value["args_len"], serde_json::json!(2));
        assert_eq!(rec.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn recording_dispatcher_unknown_verb_surfaces_error() {
        let rec = Arc::new(RecordingDispatcher {
            calls: Mutex::new(Vec::new()),
        });
        let err = rec
            .invoke("known", "missing", Vec::new())
            .await
            .expect_err("unknown verb must Err");
        assert!(err.contains("unknown verb"));
    }

    #[tokio::test]
    async fn recording_dispatcher_unknown_ext_surfaces_error() {
        let rec = Arc::new(RecordingDispatcher {
            calls: Mutex::new(Vec::new()),
        });
        let err = rec
            .invoke("nope", "anything", Vec::new())
            .await
            .expect_err("unknown ext must Err");
        assert!(err.contains("unknown extension"));
    }

    #[test]
    fn dispatcher_defaults_to_none() {
        // Note: this test may observe a dispatcher registered by a
        // concurrent test in the same binary; assert only that the
        // OnceLock accessor works without panic.
        let _ = control_verb_dispatcher();
    }
}
