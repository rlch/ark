//! Extension symbol registry (T-094).
//!
//! [`ExtensionRegistry`] is the compile-time symbol table populated by
//! `use "<ext>"` directives. Each activated extension contributes its
//! declared intents, events, and views under the `<ext-name>.*` namespace.
//! The registry supports qualified-name lookups so downstream passes
//! (op argument validation, `on` selector validation, pane `view`
//! resolution) can verify references against the known symbol set.
//!
//! # Namespace prefixing
//!
//! Extension metadata declares names in *unprefixed* form (e.g. intent
//! `"hello"` in extension `demo`). The registry stores them under the
//! fully-qualified form `"demo.hello"`. If the metadata already contains
//! a dot the name is stored as-is (the extension author opted into an
//! explicit namespace).
//!
//! # Duplicate activation
//!
//! Activating the same extension twice is idempotent — the second call
//! returns `Ok(())` without modifying the registry. This simplifies
//! transitive `use` resolution where multiple paths may converge on the
//! same extension.

use std::collections::HashMap;

use ark_ext_metadata_types::{EventDecl, ExtensionMetadata, IntentDecl, ViewDecl};

use crate::error::SceneError;

/// Reserved namespace prefix. Any extension whose name starts with this
/// is rejected at activation time via [`SceneError::ExtReservedNamespace`].
const RESERVED_PREFIX: &str = "ark.core";

/// Compile-time registry of activated extensions and their symbols.
///
/// Built up during scene composition as `use "<ext>"` nodes are
/// processed. Once fully populated the registry is read-only — the
/// compile and validation passes query it but never mutate it.
#[derive(Debug, Default)]
pub struct ExtensionRegistry {
    /// Set of activated extension names (insertion order preserved by
    /// the `Vec` in [`active_order`]).
    activated: HashMap<String, ActivatedExtension>,

    /// Insertion-order list of extension names so
    /// [`active_extensions`] returns a deterministic order.
    active_order: Vec<String>,

    /// Fully-qualified intent name -> owning extension + declaration.
    intents: HashMap<String, (String, IntentDecl)>,

    /// Fully-qualified event name -> owning extension + declaration.
    events: HashMap<String, (String, EventDecl)>,

    /// Fully-qualified view name -> owning extension + declaration.
    views: HashMap<String, (String, ViewDecl)>,
}

/// Per-extension record stored in the registry.
#[derive(Debug)]
struct ActivatedExtension {
    /// Intent names (fully qualified) contributed by this extension.
    intent_names: Vec<String>,
    /// Event names (fully qualified) contributed by this extension.
    event_names: Vec<String>,
    /// View names (fully qualified) contributed by this extension.
    view_names: Vec<String>,
    /// Full metadata snapshot retained for capability queries (T-102).
    metadata: ExtensionMetadata,
}

/// Qualify `name` under `ext_name` if it does not already contain a dot.
fn qualify(name: &str, ext_name: &str) -> String {
    if name.contains('.') {
        name.to_string()
    } else {
        format!("{ext_name}.{name}")
    }
}

impl ExtensionRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Activate an extension, registering its intents, events, and views
    /// under the `<name>.*` namespace.
    ///
    /// # Idempotency
    ///
    /// If `name` is already activated the call is a no-op and returns
    /// `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns [`SceneError::ExtReservedNamespace`] if `name` is or
    /// starts with `ark.core`.
    pub fn activate(
        &mut self,
        name: &str,
        metadata: &ExtensionMetadata,
    ) -> Result<(), SceneError> {
        // Idempotent: already activated.
        if self.activated.contains_key(name) {
            return Ok(());
        }

        // Reject reserved namespace.
        if name == RESERVED_PREFIX || name.starts_with(&format!("{RESERVED_PREFIX}.")) {
            return Err(SceneError::ExtReservedNamespace {
                ext: name.to_string(),
                attempted: name.to_string(),
            });
        }

        let mut record = ActivatedExtension {
            intent_names: Vec::new(),
            event_names: Vec::new(),
            view_names: Vec::new(),
            metadata: metadata.clone(),
        };

        // Register intents.
        for intent in &metadata.intents {
            let fqn = qualify(&intent.name, name);
            record.intent_names.push(fqn.clone());
            self.intents
                .insert(fqn, (name.to_string(), intent.clone()));
        }

        // Register events.
        for event in &metadata.events {
            let fqn = qualify(&event.name, name);
            record.event_names.push(fqn.clone());
            self.events.insert(fqn, (name.to_string(), event.clone()));
        }

        // Register views.
        for view in &metadata.views {
            let fqn = qualify(&view.name, name);
            record.view_names.push(fqn.clone());
            self.views.insert(fqn, (name.to_string(), view.clone()));
        }

        self.active_order.push(name.to_string());
        self.activated.insert(name.to_string(), record);

        Ok(())
    }

    /// Returns `true` if the named extension has been activated.
    pub fn is_active(&self, name: &str) -> bool {
        self.activated.contains_key(name)
    }

    /// Look up an intent by fully-qualified name (e.g. `"demo.hello"`).
    pub fn resolve_intent(&self, qualified_name: &str) -> Option<&IntentDecl> {
        self.intents.get(qualified_name).map(|(_, decl)| decl)
    }

    /// Look up an event by fully-qualified name (e.g. `"demo.greeted"`).
    pub fn resolve_event(&self, qualified_name: &str) -> Option<&EventDecl> {
        self.events.get(qualified_name).map(|(_, decl)| decl)
    }

    /// Look up a view by fully-qualified name (e.g. `"demo.panel"`).
    pub fn resolve_view(&self, name: &str) -> Option<&ViewDecl> {
        self.views.get(name).map(|(_, decl)| decl)
    }

    /// Return the names of all activated extensions in activation order.
    pub fn active_extensions(&self) -> Vec<&str> {
        self.active_order.iter().map(|s| s.as_str()).collect()
    }

    /// Return the stored [`ExtensionMetadata`] for an activated
    /// extension, or `None` if it has not been activated.
    pub fn metadata(&self, name: &str) -> Option<&ExtensionMetadata> {
        self.activated.get(name).map(|ae| &ae.metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ext_metadata_types::{
        CapabilitySet, ConfigSchema, EventDecl, ExtensionMetadata, IntentDecl, StringNode,
        ViewDecl,
    };

    /// Helper: build minimal extension metadata with given intents,
    /// events, and views.
    fn meta(
        name: &str,
        intents: &[(&str, &str)],
        events: &[(&str, &str)],
        views: &[(&str, &str)],
    ) -> ExtensionMetadata {
        ExtensionMetadata {
            name: StringNode::new(name),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::default(),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: intents
                .iter()
                .map(|(n, schema)| IntentDecl {
                    name: n.to_string(),
                    args_schema: StringNode::new(*schema),
                })
                .collect(),
            events: events
                .iter()
                .map(|(n, schema)| EventDecl {
                    name: n.to_string(),
                    payload_schema: StringNode::new(*schema),
                })
                .collect(),
            views: views
                .iter()
                .map(|(n, component)| ViewDecl {
                    name: n.to_string(),
                    component: StringNode::new(*component),
                })
                .collect(),
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::default(),
            config_sections: vec![],
            reload_gates: vec![],
        }
    }

    // ── Basic activation and resolution ────────────────────────────

    #[test]
    fn activate_registers_intents_events_views() {
        let mut reg = ExtensionRegistry::new();
        let m = meta(
            "demo",
            &[("hello", r#"{"type":"object"}"#)],
            &[("greeted", r#"{"type":"object"}"#)],
            &[("panel", "DemoPanel")],
        );
        reg.activate("demo", &m).unwrap();

        assert!(reg.is_active("demo"));
        assert!(reg.resolve_intent("demo.hello").is_some());
        assert!(reg.resolve_event("demo.greeted").is_some());
        assert!(reg.resolve_view("demo.panel").is_some());
    }

    // ── Namespace prefixing ────────────────────────────────────────

    #[test]
    fn unprefixed_names_are_qualified() {
        let mut reg = ExtensionRegistry::new();
        let m = meta("git", &[("commit", "{}"), ("push", "{}")], &[], &[]);
        reg.activate("git", &m).unwrap();

        assert!(reg.resolve_intent("git.commit").is_some());
        assert!(reg.resolve_intent("git.push").is_some());
        // Bare name should NOT resolve.
        assert!(reg.resolve_intent("commit").is_none());
    }

    #[test]
    fn already_qualified_names_stored_as_is() {
        let mut reg = ExtensionRegistry::new();
        let m = meta("git", &[("git.commit", "{}")], &[], &[]);
        reg.activate("git", &m).unwrap();

        assert!(reg.resolve_intent("git.commit").is_some());
    }

    // ── Duplicate activation is idempotent ─────────────────────────

    #[test]
    fn duplicate_activation_is_idempotent() {
        let mut reg = ExtensionRegistry::new();
        let m = meta("demo", &[("hello", "{}")], &[], &[]);

        reg.activate("demo", &m).unwrap();
        // Second activation is a no-op.
        reg.activate("demo", &m).unwrap();

        assert_eq!(reg.active_extensions().len(), 1);
        assert!(reg.resolve_intent("demo.hello").is_some());
    }

    // ── Reserved namespace rejection ───────────────────────────────

    #[test]
    fn rejects_ark_core_extension() {
        let mut reg = ExtensionRegistry::new();
        let m = meta("ark.core", &[], &[], &[]);

        let err = reg.activate("ark.core", &m).unwrap_err();
        match err {
            SceneError::ExtReservedNamespace { ext, .. } => {
                assert_eq!(ext, "ark.core");
            }
            other => panic!("expected ExtReservedNamespace, got: {other:?}"),
        }
    }

    #[test]
    fn rejects_ark_core_sub_namespace() {
        let mut reg = ExtensionRegistry::new();
        let m = meta("ark.core.evil", &[], &[], &[]);

        let err = reg.activate("ark.core.evil", &m).unwrap_err();
        match err {
            SceneError::ExtReservedNamespace { ext, .. } => {
                assert_eq!(ext, "ark.core.evil");
            }
            other => panic!("expected ExtReservedNamespace, got: {other:?}"),
        }
    }

    // ── active_extensions returns insertion order ───────────────────

    #[test]
    fn active_extensions_preserves_insertion_order() {
        let mut reg = ExtensionRegistry::new();
        reg.activate("alpha", &meta("alpha", &[], &[], &[]))
            .unwrap();
        reg.activate("beta", &meta("beta", &[], &[], &[]))
            .unwrap();
        reg.activate("gamma", &meta("gamma", &[], &[], &[]))
            .unwrap();

        assert_eq!(reg.active_extensions(), vec!["alpha", "beta", "gamma"]);
    }

    // ── Unactivated extension is not active ─────────────────────────

    #[test]
    fn is_active_false_for_unknown() {
        let reg = ExtensionRegistry::new();
        assert!(!reg.is_active("nope"));
    }

    // ── Resolution returns None for unknown symbols ─────────────────

    #[test]
    fn resolve_returns_none_for_unknown() {
        let reg = ExtensionRegistry::new();
        assert!(reg.resolve_intent("demo.hello").is_none());
        assert!(reg.resolve_event("demo.greeted").is_none());
        assert!(reg.resolve_view("demo.panel").is_none());
    }

    // ── Multiple extensions with distinct symbols ───────────────────

    #[test]
    fn multiple_extensions_coexist() {
        let mut reg = ExtensionRegistry::new();
        reg.activate(
            "git",
            &meta("git", &[("commit", "{}")], &[("pushed", "{}")], &[]),
        )
        .unwrap();
        reg.activate(
            "ai",
            &meta(
                "ai",
                &[("prompt", "{}")],
                &[("responded", "{}")],
                &[("chat", "ChatView")],
            ),
        )
        .unwrap();

        assert!(reg.resolve_intent("git.commit").is_some());
        assert!(reg.resolve_intent("ai.prompt").is_some());
        assert!(reg.resolve_event("git.pushed").is_some());
        assert!(reg.resolve_event("ai.responded").is_some());
        assert!(reg.resolve_view("ai.chat").is_some());

        // Cross-namespace misses.
        assert!(reg.resolve_intent("ai.commit").is_none());
        assert!(reg.resolve_view("git.chat").is_none());
    }

    // ── Empty metadata is fine ──────────────────────────────────────

    #[test]
    fn empty_metadata_activates_cleanly() {
        let mut reg = ExtensionRegistry::new();
        reg.activate("empty", &meta("empty", &[], &[], &[]))
            .unwrap();

        assert!(reg.is_active("empty"));
        assert_eq!(reg.active_extensions(), vec!["empty"]);
    }
}
