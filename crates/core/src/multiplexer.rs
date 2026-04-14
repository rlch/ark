//! Multiplexer trait — abstract terminal multiplexer hosting agent panes.
//!
//! Implements cavekit-architecture.md R4. v1 ships one impl (`ZellijMux`,
//! delivered later); the trait surface is intentionally tmux-compatible so a
//! `TmuxMux` can land without churning the trait.

use std::path::Path;

use ark_types::TabHandle;
use async_trait::async_trait;

/// Abstract multiplexer interface. See cavekit-architecture.md R4.
#[async_trait]
pub trait Multiplexer: Send + Sync {
    /// Stable slug identifying this multiplexer (e.g. `"zellij"`).
    fn kind(&self) -> &'static str;

    /// Idempotently ensure a session exists with the given name. Returns
    /// `Ok(())` whether the session was created or already running.
    async fn ensure_session(&self, name: &str) -> anyhow::Result<()>;

    /// Open a new tab in `session` named `name`, using the KDL/layout file at
    /// `layout_path`. Returns a [`TabHandle`] the caller can use to close /
    /// rename the tab later.
    async fn create_tab(
        &self,
        session: &str,
        name: &str,
        layout_path: &Path,
    ) -> anyhow::Result<TabHandle>;

    /// Close the tab identified by `handle`. Idempotent: closing an
    /// already-closed tab is `Ok(())`.
    async fn close_tab(&self, handle: &TabHandle) -> anyhow::Result<()>;

    /// Rename the tab identified by `handle` to `name`.
    async fn rename_tab(&self, handle: &TabHandle, name: &str) -> anyhow::Result<()>;

    /// Send `payload` to a named pipe target (e.g. a wasm plugin's pipe
    /// listener). Used for status/picker plugin updates.
    async fn pipe(&self, target_name: &str, payload: &str) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// Simple recording mock used to verify trait-object dispatch and the
    /// expected method-call sequence.
    struct MockMux {
        calls: Mutex<Vec<String>>,
    }

    impl MockMux {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }

        fn record(&self, s: impl Into<String>) {
            self.calls.lock().unwrap().push(s.into());
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Multiplexer for MockMux {
        fn kind(&self) -> &'static str {
            "mock"
        }

        async fn ensure_session(&self, name: &str) -> anyhow::Result<()> {
            self.record(format!("ensure_session:{name}"));
            Ok(())
        }

        async fn create_tab(
            &self,
            session: &str,
            name: &str,
            layout_path: &Path,
        ) -> anyhow::Result<TabHandle> {
            self.record(format!(
                "create_tab:{session}:{name}:{}",
                layout_path.display()
            ));
            Ok(TabHandle::new(session, 1, name))
        }

        async fn close_tab(&self, handle: &TabHandle) -> anyhow::Result<()> {
            self.record(format!("close_tab:{}", handle.name));
            Ok(())
        }

        async fn rename_tab(&self, handle: &TabHandle, name: &str) -> anyhow::Result<()> {
            self.record(format!("rename_tab:{}->{}", handle.name, name));
            Ok(())
        }

        async fn pipe(&self, target_name: &str, payload: &str) -> anyhow::Result<()> {
            self.record(format!("pipe:{target_name}:{payload}"));
            Ok(())
        }
    }

    #[tokio::test]
    async fn mock_mux_records_create_then_close_sequence() {
        let mux = MockMux::new();
        mux.ensure_session("ark-cavekit-auth").await.unwrap();
        let layout = PathBuf::from("/tmp/layout.kdl");
        let handle = mux
            .create_tab("ark-cavekit-auth", "builder", &layout)
            .await
            .unwrap();
        assert_eq!(handle.name, "builder");
        assert_eq!(handle.session, "ark-cavekit-auth");
        mux.rename_tab(&handle, "builder*").await.unwrap();
        mux.pipe("ark-status", "{\"k\":\"v\"}").await.unwrap();
        mux.close_tab(&handle).await.unwrap();

        let calls = mux.calls();
        assert_eq!(
            calls,
            vec![
                "ensure_session:ark-cavekit-auth".to_string(),
                "create_tab:ark-cavekit-auth:builder:/tmp/layout.kdl".to_string(),
                "rename_tab:builder->builder*".to_string(),
                "pipe:ark-status:{\"k\":\"v\"}".to_string(),
                "close_tab:builder".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn trait_object_dispatch_via_arc() {
        let mux: Arc<dyn Multiplexer> = Arc::new(MockMux::new());
        assert_eq!(mux.kind(), "mock");
        mux.ensure_session("s").await.unwrap();
        let h = mux
            .create_tab("s", "t", Path::new("/tmp/x.kdl"))
            .await
            .unwrap();
        mux.close_tab(&h).await.unwrap();
    }
}
