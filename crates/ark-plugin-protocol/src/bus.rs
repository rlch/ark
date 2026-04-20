//! Pipe/intent bus types.
//!
//! T-PP-010 (cavekit-plugin-protocol R11): the pipe bus is how plugins
//! talk to each other and to the CLI. Structural types only at this
//! tier — routing + cascade-depth enforcement lives in `ark-host`
//! (T-PP-043 onwards).

/// A named intent + opaque payload, routed through the pipe bus.
///
/// `payload` is a postcard-encoded blob — plugins that want typed
/// intents declare them via the SDK (Tier 2) and encode/decode through
/// `postcard`. The host treats payloads as opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// Intent name — a bare identifier matching `[a-z][a-z0-9-]*` as
    /// validated by the host on send.
    pub name: String,
    /// Postcard-encoded payload (opaque to the host).
    pub payload: Vec<u8>,
}

/// Routing target for an [`Intent`] sent via the pipe bus.
///
/// `#[non_exhaustive]` so post-v1 additions (e.g. `Session(id)` or
/// `Scene(handle)`) are MINOR ABI bumps, not MAJOR.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IntentTarget {
    /// Broadcast to every plugin that subscribes to this intent name.
    Broadcast,
    /// Send to exactly one plugin by its URL identity (`file:...`,
    /// `https:...`, `oci:...`).
    Plugin(String),
    /// Send to a handle `@h` registered via the scene graph.
    Handle(String),
}

/// Origin of an intent on the pipe bus. Used for cap-checks (the CLI
/// source is always trusted, plugin sources are checked against their
/// granted caps, …) and for diagnostics.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipeSource {
    /// `ark pipe <intent>` or equivalent CLI invocation.
    Cli,
    /// A plugin with the given URL identity emitted the intent.
    Plugin {
        /// URL of the sending plugin (`file:./foo.wasm`, …).
        url: String,
    },
    /// A keybind chord (e.g. `ctrl-p p`) emitted the intent.
    Keybind {
        /// The chord string as configured in `ark.kdl`.
        chord: String,
    },
}

/// A single message on the pipe bus: intent + source + target.
///
/// Structural only — queueing / fan-out / cascade tracking live in
/// `ark-host` (Tier 3+).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeMessage {
    pub intent: Intent,
    pub source: PipeSource,
    pub target: IntentTarget,
}

/// Errors raised by the host while routing a [`PipeMessage`].
///
/// `#[non_exhaustive]` — post-v1 additions (cycle detection, rate
/// limits, …) are MINOR ABI bumps.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BusError {
    /// An intent's fan-out chain exceeded the host's configured depth
    /// cap.
    ///
    /// Kit: R11 acceptance "host enforces a cap to prevent cascades".
    #[error("bus: cascade depth exceeded")]
    CascadeDepthExceeded,

    /// The target in the message does not resolve to any loaded
    /// plugin / handle / subscriber.
    ///
    /// Kit: R11 acceptance "unknown target surfaces a typed error".
    #[error("bus: message target is unroutable")]
    Unroutable,

    /// The sending plugin lacks the `bus-send` cap (or the receiver
    /// lacks `bus-receive`).
    ///
    /// Kit: R11 acceptance "bus-send/bus-receive are caps".
    #[error("bus: capability not granted for this source")]
    CapNotGranted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_target_variants_round_trip() {
        let a = IntentTarget::Broadcast;
        let b = IntentTarget::Plugin("file:./foo.wasm".into());
        let c = IntentTarget::Handle("@picker".into());
        assert_ne!(a, b);
        assert_ne!(b, c);
    }

    #[test]
    fn pipe_source_variants_cover_kit() {
        // R11 names these three — verify they all construct.
        let _ = PipeSource::Cli;
        let _ = PipeSource::Plugin {
            url: "file:./p.wasm".into(),
        };
        let _ = PipeSource::Keybind {
            chord: "ctrl-p p".into(),
        };
    }

    #[test]
    fn bus_error_display_nonempty() {
        for e in [
            BusError::CascadeDepthExceeded,
            BusError::Unroutable,
            BusError::CapNotGranted,
        ] {
            assert!(!format!("{e}").is_empty());
        }
    }

    #[test]
    fn pipe_message_assembles() {
        let m = PipeMessage {
            intent: Intent {
                name: "save".into(),
                payload: vec![1, 2, 3],
            },
            source: PipeSource::Cli,
            target: IntentTarget::Broadcast,
        };
        assert_eq!(m.intent.payload, vec![1, 2, 3]);
    }
}
