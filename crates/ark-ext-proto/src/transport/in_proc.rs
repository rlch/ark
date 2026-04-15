//! In-process trait-object dispatcher for compiled-in extensions.
//!
//! Placeholder module — T-9.5.4 fills in the full [`InProcClient`]
//! implementation that wraps an `Arc<dyn ArkExtension>` and dispatches
//! directly without JSON serialization cost.

/// Zero-overhead [`super::ExtensionClient`] backed by a shared
/// `Arc<dyn ArkExtension>`. Filled in by T-9.5.4.
pub struct InProcClient {
    _placeholder: (),
}
