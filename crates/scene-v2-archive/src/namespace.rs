//! Context-sensitive namespace rewriting (cavekit-scene R11, T-10.7).
//!
//! Scene authors write intent and event names in two forms:
//!
//! * **Fully-qualified** — the name already carries a namespace segment
//!   (e.g. `ark.core.open_tab`, `picker.show`, `user.hello`). Always
//!   preserved verbatim.
//! * **Unprefixed** — a bare identifier (e.g. `open_tab`, `show`). The
//!   meaning depends on *where* the author wrote it:
//!   - inside a **user scene**, unprefixed names belong to the author's
//!     own `user.*` namespace: `foo` → `user.foo`.
//!   - inside an **extension's sidecar scene fragment**, unprefixed
//!     names belong to the extension's own namespace:
//!     `show` in the picker extension → `picker.show`.
//!
//! Core ops (`ark.core.*`) are NEVER the target of an unprefixed
//! rewrite. Extension authors who want to call `ark.core.open_tab`
//! MUST write the fully-qualified name — otherwise the rewrite would
//! silently shadow the host's core op set, a Python-style import-shadow
//! footgun this module deliberately forbids.
//!
//! The `ark.core.*` namespace itself is reserved: an extension named
//! `ark-core` (or any shape that would cause the rewrite to emit
//! `ark.core.<x>`) trips [`SceneError::ReservedNamespace`] (code
//! `ext/reserved-namespace`).
//!
//! # Scope
//!
//! The rewrite operates on intent/event *name strings* — the values
//! passed to `keybind intent="…"`, `on "UserEvent:…"`, `emit "…"`, and
//! on the `name` fields of intent/event declarations contributed by an
//! extension manifest. The compose pass walks every such site and
//! applies [`rewrite_intent`] to the string it found.
//!
//! The module deliberately exposes only pure functions. The compile
//! pipeline owns the AST walk; this file owns the rule.
//!
//! # Examples
//!
//! ```
//! use ark_scene_v2_archive::namespace::{rewrite_intent, NamespaceOrigin};
//!
//! // User-scene site: bare name → user.*
//! assert_eq!(
//!     rewrite_intent("hello", &NamespaceOrigin::UserScene).unwrap(),
//!     "user.hello"
//! );
//!
//! // Extension-fragment site: bare name → <ext>.*
//! let origin = NamespaceOrigin::Extension("picker".into());
//! assert_eq!(rewrite_intent("show", &origin).unwrap(), "picker.show");
//!
//! // Already-qualified name passes through untouched.
//! assert_eq!(
//!     rewrite_intent("ark.core.open_tab", &NamespaceOrigin::UserScene).unwrap(),
//!     "ark.core.open_tab"
//! );
//! ```

use ark_ext_metadata_types::ExtensionMetadata;

use crate::error::SceneError;

/// Reserved namespace prefix — core ops live under `ark.core.*` and
/// extensions MUST NOT rewrite into it (R11).
pub const RESERVED_CORE_PREFIX: &str = "ark.core";

/// Where a name was encountered during the scene compile pass.
///
/// Drives the unprefixed-rewrite rule: user-scene sites prepend
/// `user.`, extension-fragment sites prepend `<ext>.`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceOrigin {
    /// Name came from a user-authored scene file (the top-level scene,
    /// anything included/extended from it).
    UserScene,
    /// Name came from an extension's sidecar `scene.kdl` fragment, or
    /// from an intent/event declaration in the extension's manifest.
    /// The inner string is the extension's name (e.g. `"picker"`).
    Extension(String),
}

impl NamespaceOrigin {
    /// Return the prefix this origin contributes to unprefixed names
    /// (without the trailing `.`). Used by [`rewrite_intent`] and by
    /// [`is_reserved_namespace`] to reason about whether the origin's
    /// rewrite would land inside the reserved core prefix.
    pub fn namespace_prefix(&self) -> &str {
        match self {
            NamespaceOrigin::UserScene => "user",
            NamespaceOrigin::Extension(ext) => ext.as_str(),
        }
    }
}

/// Context-sensitive rewrite of an intent / event name.
///
/// Rules (R11):
///
/// 1. If `name` already contains a `.`, it's considered namespaced and
///    returned unchanged.
/// 2. Otherwise, the origin's prefix is prepended:
///    - [`NamespaceOrigin::UserScene`] → `user.<name>`
///    - [`NamespaceOrigin::Extension(e)`] → `<e>.<name>`
/// 3. If the origin itself is `ark.core` (i.e. an extension whose
///    name would rewrite into the reserved prefix), return
///    [`SceneError::ReservedNamespace`].
///
/// # Errors
///
/// Returns [`SceneError::ReservedNamespace`] (code `ext/reserved-namespace`)
/// when an extension's namespace-prefix is literally `ark.core`. This
/// variant fires both on unprefixed names (where the rewrite would
/// land in the reserved prefix) and on already-qualified names
/// starting with `ark.core.` contributed from an extension.
#[allow(clippy::result_large_err)]
pub fn rewrite_intent(
    name: &str,
    origin: &NamespaceOrigin,
) -> Result<String, SceneError> {
    // Reserved-namespace check: an extension literally named `ark-core`
    // (or whose manifest-declared name maps to `ark.core`) is never a
    // valid origin for the rewrite. Catch it before the rewrite lookup
    // so the error message identifies the offending extension.
    if let NamespaceOrigin::Extension(ext) = origin {
        if ext_maps_to_reserved(ext) {
            return Err(SceneError::ReservedNamespace {
                ext: ext.clone(),
                attempted: format!("{RESERVED_CORE_PREFIX}.{name}"),
            });
        }
    }

    if is_already_namespaced(name) {
        // Already-qualified names pass through verbatim — an extension
        // referencing `ark.core.open_tab` is fine (that's how extensions
        // call into host ops per R11). The reserved-namespace check
        // above only guards against extensions whose OWN namespace
        // collides with the reserved prefix.
        return Ok(name.to_string());
    }

    let prefix = origin.namespace_prefix();
    Ok(format!("{prefix}.{name}"))
}

/// Returns `true` iff the rewrite produced by this origin would land
/// inside the reserved `ark.core.*` namespace.
///
/// Useful for callers that want to validate an extension's origin
/// up-front without running [`rewrite_intent`] on every declaration.
pub fn is_reserved_namespace(origin: &NamespaceOrigin) -> bool {
    match origin {
        NamespaceOrigin::UserScene => false,
        NamespaceOrigin::Extension(ext) => ext_maps_to_reserved(ext),
    }
}

/// Returns `true` iff the name already has a `.` separator — our
/// "fully qualified" heuristic.
///
/// Extension authors who forget the dot get the rewrite applied; the
/// `ark.core.open_tab` spelling requires the explicit dot. This matches
/// the cavekit-scene.md R11 wording "Already-namespaced … → unchanged".
fn is_already_namespaced(name: &str) -> bool {
    name.contains('.')
}

/// Returns `true` if the given extension name, after normalization,
/// would map to the reserved `ark.core` prefix.
///
/// Accepts both the dotted form (`ark.core`) and the dashed form an
/// extension author might write (`ark-core`) — both would trip the
/// rewrite if allowed, so both are rejected up-front.
fn ext_maps_to_reserved(ext: &str) -> bool {
    ext == RESERVED_CORE_PREFIX || ext == "ark-core"
}

/// Validate that an extension's manifest-declared intents and events
/// do not collide with the reserved `ark.core.*` namespace.
///
/// Called by the scene compose pass after each `use "<ext>"` resolves.
/// When the extension's name itself maps to `ark.core`, every
/// declaration is flagged via [`SceneError::ReservedNamespace`].
/// When the extension's name is benign but a declaration is written
/// in the fully-qualified `ark.core.<foo>` form (e.g. an extension
/// trying to pretend its intent lives under the core namespace), the
/// same error fires with the extension's own name as `ext` so the
/// user knows who to blame.
///
/// Returns the first violation; callers that want to collect all
/// violations can iterate through the manifest's vectors themselves
/// using [`rewrite_intent`].
#[allow(clippy::result_large_err)]
pub fn validate_extension_namespace(
    ext_name: &str,
    metadata: &ExtensionMetadata,
) -> Result<(), SceneError> {
    let origin = NamespaceOrigin::Extension(ext_name.to_string());

    // Origin-level reserved-name check — covers the case where the
    // extension's OWN namespace collides with `ark.core`.
    if is_reserved_namespace(&origin) {
        return Err(SceneError::ReservedNamespace {
            ext: ext_name.to_string(),
            attempted: format!("{RESERVED_CORE_PREFIX}.*"),
        });
    }

    // Declaration-level reserved-name check — catches an extension
    // trying to reach into the reserved prefix by writing fully-
    // qualified `ark.core.<foo>` as the intent/event NAME. The
    // forbidden shape here is an extension contributing a
    // *declaration* under `ark.core.*`; extensions may freely
    // REFERENCE `ark.core.*` names as targets, which is why this
    // check lives here (at manifest-decl time) rather than inside
    // `rewrite_intent` itself.
    for intent in &metadata.intents {
        if is_declaration_in_reserved(&intent.name) {
            return Err(SceneError::ReservedNamespace {
                ext: ext_name.to_string(),
                attempted: intent.name.clone(),
            });
        }
    }
    for event in &metadata.events {
        if is_declaration_in_reserved(&event.name) {
            return Err(SceneError::ReservedNamespace {
                ext: ext_name.to_string(),
                attempted: event.name.clone(),
            });
        }
    }

    Ok(())
}

/// Returns `true` iff `name`, taken as an extension's *declaration*,
/// would land inside the reserved `ark.core.*` namespace.
///
/// Distinct from the rewrite path: declarations always land in the
/// extension's own namespace, so a decl under `ark.core.*` is always
/// a reservation violation (even when already-qualified).
fn is_declaration_in_reserved(name: &str) -> bool {
    name == RESERVED_CORE_PREFIX
        || name.starts_with(&format!("{RESERVED_CORE_PREFIX}."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn user_scene_bare_name_prefixed_with_user() {
        let got = rewrite_intent("hello", &NamespaceOrigin::UserScene).unwrap();
        assert_eq!(got, "user.hello");
    }

    #[test]
    fn extension_bare_name_prefixed_with_ext() {
        let origin = NamespaceOrigin::Extension("picker".into());
        assert_eq!(rewrite_intent("show", &origin).unwrap(), "picker.show");
    }

    #[test]
    fn already_namespaced_user_intent_passes_through() {
        let got =
            rewrite_intent("user.custom", &NamespaceOrigin::UserScene).unwrap();
        assert_eq!(got, "user.custom");
    }

    #[test]
    fn already_namespaced_core_intent_passes_through_in_user_scene() {
        let got = rewrite_intent(
            "ark.core.open_tab",
            &NamespaceOrigin::UserScene,
        )
        .unwrap();
        assert_eq!(got, "ark.core.open_tab");
    }

    #[test]
    fn extension_can_reference_ark_core_when_fully_qualified() {
        // Extension authors MUST write the full `ark.core.*` name to
        // reach a core op — no unprefixed rewrite for core ops.
        let origin = NamespaceOrigin::Extension("picker".into());
        assert_eq!(
            rewrite_intent("ark.core.open_tab", &origin).unwrap(),
            "ark.core.open_tab"
        );
    }

    #[test]
    fn extension_bare_name_does_not_fall_through_to_core() {
        // Guard against the import-shadow footgun: a bare `open_tab`
        // from an extension MUST NOT become `ark.core.open_tab`; it
        // becomes `<ext>.open_tab`.
        let origin = NamespaceOrigin::Extension("picker".into());
        let got = rewrite_intent("open_tab", &origin).unwrap();
        assert_eq!(got, "picker.open_tab");
        assert!(!got.starts_with("ark.core"));
    }

    #[test]
    fn reserved_namespace_ark_core_direct_errors() {
        let origin = NamespaceOrigin::Extension("ark.core".into());
        let err =
            rewrite_intent("foo", &origin).expect_err("reserved");
        assert_eq!(err.code_enum(), ErrorCode::ReservedNamespace);
        match err {
            SceneError::ReservedNamespace { ext, attempted } => {
                assert_eq!(ext, "ark.core");
                assert_eq!(attempted, "ark.core.foo");
            }
            other => panic!("expected ReservedNamespace, got {other:?}"),
        }
    }

    #[test]
    fn reserved_namespace_ark_core_dashed_errors() {
        // An extension dir named `ark-core` maps to the same reserved
        // namespace through the scene-compiler's normalisation.
        let origin = NamespaceOrigin::Extension("ark-core".into());
        let err =
            rewrite_intent("x", &origin).expect_err("reserved");
        assert_eq!(err.code_enum(), ErrorCode::ReservedNamespace);
    }

    #[test]
    fn extension_can_reference_core_op_by_fully_qualified_name() {
        // Extensions can dispatch to core ops by writing the full
        // `ark.core.*` name. The reserved-namespace check only fires
        // when the extension's OWN namespace collides — an extension
        // named `myext` referencing `ark.core.open_tab` is a normal
        // cross-namespace call.
        let origin = NamespaceOrigin::Extension("myext".into());
        let got = rewrite_intent("ark.core.open_tab", &origin).unwrap();
        assert_eq!(got, "ark.core.open_tab");
    }

    #[test]
    fn user_scene_with_ark_core_name_is_fine() {
        // The user DID write the fully-qualified core name in a user
        // scene — that's the only way to dispatch a core op. No error.
        let got = rewrite_intent(
            "ark.core.split_pane",
            &NamespaceOrigin::UserScene,
        )
        .unwrap();
        assert_eq!(got, "ark.core.split_pane");
    }

    #[test]
    fn is_reserved_namespace_classifies_origins() {
        assert!(!is_reserved_namespace(&NamespaceOrigin::UserScene));
        assert!(!is_reserved_namespace(&NamespaceOrigin::Extension(
            "picker".into()
        )));
        assert!(is_reserved_namespace(&NamespaceOrigin::Extension(
            "ark.core".into()
        )));
        assert!(is_reserved_namespace(&NamespaceOrigin::Extension(
            "ark-core".into()
        )));
    }

    #[test]
    fn namespace_prefix_returns_expected_strings() {
        assert_eq!(NamespaceOrigin::UserScene.namespace_prefix(), "user");
        assert_eq!(
            NamespaceOrigin::Extension("picker".into()).namespace_prefix(),
            "picker"
        );
    }

    #[test]
    fn hyphenated_extension_name_used_verbatim() {
        // Extension names may contain hyphens; the rewrite uses the
        // name verbatim (no dash-to-dot transform). Valid KDL-identifier
        // shape.
        let origin = NamespaceOrigin::Extension("engine-claude".into());
        let got = rewrite_intent("launch", &origin).unwrap();
        assert_eq!(got, "engine-claude.launch");
    }

    #[test]
    fn dotted_selector_like_name_is_not_further_rewritten() {
        // `picker.show` — already has a dot → namespaced → unchanged.
        let origin = NamespaceOrigin::Extension("picker".into());
        assert_eq!(
            rewrite_intent("picker.show", &origin).unwrap(),
            "picker.show"
        );
    }

    // -- validate_extension_namespace -----------------------------------

    use ark_ext_metadata_types::{
        CapabilitySet, ConfigSchema, EventDecl, ExtensionMetadata, IntentDecl, StringNode,
    };

    fn meta_with(intents: Vec<&str>, events: Vec<&str>) -> ExtensionMetadata {
        ExtensionMetadata {
            name: StringNode::new("x"),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::default(),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: intents
                .into_iter()
                .map(|n| IntentDecl {
                    name: n.into(),
                    args_schema: StringNode::new("{}"),
                })
                .collect(),
            events: events
                .into_iter()
                .map(|n| EventDecl {
                    name: n.into(),
                    payload_schema: StringNode::new("{}"),
                })
                .collect(),
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::default(),
        }
    }

    #[test]
    fn validate_accepts_benign_manifest() {
        let m = meta_with(vec!["show", "picker.hide"], vec!["picked"]);
        validate_extension_namespace("picker", &m).unwrap();
    }

    #[test]
    fn validate_rejects_ark_core_named_extension() {
        let m = meta_with(vec!["foo"], vec![]);
        let err = validate_extension_namespace("ark.core", &m)
            .expect_err("reserved name rejected");
        assert_eq!(err.code_enum(), ErrorCode::ReservedNamespace);
    }

    #[test]
    fn validate_rejects_intent_declared_under_ark_core() {
        let m = meta_with(vec!["ark.core.open_tab"], vec![]);
        let err = validate_extension_namespace("myext", &m)
            .expect_err("intent collision");
        assert_eq!(err.code_enum(), ErrorCode::ReservedNamespace);
        match err {
            SceneError::ReservedNamespace { ext, attempted } => {
                assert_eq!(ext, "myext");
                assert_eq!(attempted, "ark.core.open_tab");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn validate_rejects_event_declared_under_ark_core() {
        let m = meta_with(vec![], vec!["ark.core.tab_opened"]);
        let err = validate_extension_namespace("myext", &m)
            .expect_err("event collision");
        assert_eq!(err.code_enum(), ErrorCode::ReservedNamespace);
    }
}
