//! Figment-driven extension config-section loader (T-033).
//!
//! Extensions declare named config sub-sections via
//! [`ExtensionMetadata.config_sections`] (T-003). This module loads each
//! declared section from the layered figment provider under the key
//! path `extension.<ext-name>.<section>`.
//!
//! Per `cavekit-soul-phase-2-host-dispatch.md` R3 ("figment config-section
//! layering"). T-033 scope is the layering mechanism + missing-detection
//! only: deserialisation into section-specific types is the consumer's
//! responsibility, and schema-driven validation (beyond a single
//! `"required": true` top-level flag) is deferred to a later pass once
//! the reflection-driven config pipeline lands.
//!
//! # Precedence
//!
//! `extract_section` operates on a pre-built layered [`figment::Figment`]
//! (typically the one produced by [`crate::ConfigLoader::load`]). Callers
//! are expected to assemble the full layer chain (defaults → user →
//! project → env → overrides) before calling into this module — the
//! module does not construct or re-order figments.
//!
//! # Error shape
//!
//! Section-presence errors surface as [`ExtConfigError::MissingRequired`]
//! carrying the offending `(ext_name, section)` pair so `ark doctor` and
//! boot-time diagnostics can point at the precise key that needs to be
//! set. Deserialisation errors raised by consumers live on a different
//! surface and are not T-033's concern.

use ark_ext_metadata_types::{ConfigSectionDecl, ExtensionMetadata};

/// Errors produced by the extension config-section loader.
///
/// `MissingRequired` is raised at boot when an extension's manifest
/// declares a config section whose schema carries `"required": true` and
/// the layered figment does not provide a value for the corresponding
/// `extension.<ext-name>.<section>` key.
#[derive(Debug, thiserror::Error)]
pub enum ExtConfigError {
    /// The manifest marks the section as required, but no layer in the
    /// figment chain supplied a value for it.
    #[error(
        "extension {ext_name:?} requires config section {section:?} but it was not provided"
    )]
    MissingRequired {
        /// Extension name (the directory-name/manifest `name`).
        ext_name: String,
        /// Section identifier from [`ConfigSectionDecl::name`].
        section: String,
    },
}

/// Build the figment key for a given extension section.
///
/// Layered as `extension.<ext-name>.<section>`, matching the scheme
/// documented in `cavekit-soul-phase-2-host-dispatch.md` R3. Exposed so
/// error messages / diagnostics in callers can reference the exact key
/// without reconstructing the format string by hand.
pub fn section_key(ext_name: &str, section: &str) -> String {
    format!("extension.{ext_name}.{section}")
}

/// Load a named section for an extension from a pre-built figment.
///
/// Returns `Ok(Some(value))` when the figment chain supplies a value at
/// `extension.<ext-name>.<section>`, `Ok(None)` when the key is absent
/// (any layer), or `Err(_)` when figment itself errored (malformed TOML,
/// env parse failure, etc.).
///
/// Callers interpret `None` per the [`ConfigSectionDecl`]'s required-ness:
/// a required section returning `None` is a boot error; an optional
/// section falls back to the extension's default.
///
/// The section body is returned as a [`serde_json::Value`] so consumers
/// can route it into their section-specific facet/SHAPE deserialiser
/// without re-rendering the figment layer chain.
pub fn extract_section(
    figment: &figment::Figment,
    ext_name: &str,
    section: &str,
) -> Result<Option<serde_json::Value>, figment::Error> {
    let key = section_key(ext_name, section);
    match figment.extract_inner::<serde_json::Value>(&key) {
        Ok(val) => Ok(Some(val)),
        Err(e) if e.missing() => Ok(None),
        Err(e) => Err(e),
    }
}

/// Parse `decl.schema.value` as JSON and inspect its top-level
/// `"required"` flag.
///
/// Treated as a best-effort check: sections whose schema is malformed
/// JSON, or whose top-level is not an object, are assumed to be optional.
/// Schema-driven validation (full JSON-Schema enforcement) is out of
/// T-033 scope — see module docs.
fn section_is_required(decl: &ConfigSectionDecl) -> bool {
    let schema_str = &decl.schema.value;
    serde_json::from_str::<serde_json::Value>(schema_str)
        .ok()
        .and_then(|v| v.get("required").and_then(|r| r.as_bool()))
        .unwrap_or(false)
}

/// Validate that every entry in `sections` is satisfied by the layered
/// figment for extension `ext_name`.
///
/// Returns `Ok(())` when every required section is present (optional
/// sections may be absent) and `Err(ExtConfigError::MissingRequired)` on
/// the first missing-required hit. Stops at the first error — callers
/// that want full diagnostics can iterate themselves using
/// [`extract_section`].
///
/// "Required" is determined by parsing the section's JSON-Schema and
/// reading the top-level `"required": true` key (see
/// [`section_is_required`]). For v0.1 this is an explicit boolean flag;
/// once the reflection pipeline lands in a later tier this will switch
/// to a full schema walk.
///
/// # Errors
///
/// Returns [`ExtConfigError::MissingRequired`] when a required section is
/// missing from the figment. Does NOT surface figment extraction errors:
/// we treat "figment threw on a required key" as "section is present but
/// deserialisation is the consumer's problem" — the presence check only
/// asks "is there SOMETHING here?".
pub fn validate_ext_sections(
    figment: &figment::Figment,
    ext_name: &str,
    sections: &[ConfigSectionDecl],
) -> Result<(), ExtConfigError> {
    for decl in sections {
        let section_name = &decl.name;
        if !section_is_required(decl) {
            continue;
        }

        // Presence only — a figment error on the key (e.g. deserialization
        // failure if the value exists but isn't valid JSON) still means
        // "something is there", so we treat only `MissingField` as absent.
        let present = match figment.extract_inner::<serde_json::Value>(&section_key(
            ext_name,
            section_name,
        )) {
            Ok(_) => true,
            Err(e) if e.missing() => false,
            Err(_) => true,
        };

        if !present {
            return Err(ExtConfigError::MissingRequired {
                ext_name: ext_name.to_string(),
                section: section_name.clone(),
            });
        }
    }
    Ok(())
}

/// Validate every extension's declared sections against the figment chain.
///
/// Iterates `(ext_name, metadata)` tuples in order and calls
/// [`validate_ext_sections`] on each, stopping at the first missing-
/// required error. Extension order matters: the error surfaces for
/// whichever extension is checked first. Callers that want extension-
/// order-independent diagnostics can call [`validate_ext_sections`]
/// per-extension and collect errors.
///
/// # Errors
///
/// Propagates the first [`ExtConfigError::MissingRequired`] raised by any
/// extension's validation pass.
pub fn validate_all_extensions(
    figment: &figment::Figment,
    manifests: &[(String, ExtensionMetadata)],
) -> Result<(), ExtConfigError> {
    for (ext_name, metadata) in manifests {
        validate_ext_sections(figment, ext_name, &metadata.config_sections)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ext_metadata_types::{
        CapabilitySet, ConfigSchema, ConfigSectionDecl, ExtensionMetadata, StringNode,
    };
    use figment::Figment;
    use figment::providers::{Format, Toml};

    /// Build a minimal `ConfigSectionDecl` carrying the given name +
    /// schema-json. `schema` is a raw JSON-Schema string — tests pass
    /// `"required": true` inside when they want the section flagged
    /// required.
    fn sample_decl(name: &str, schema_json: &str) -> ConfigSectionDecl {
        ConfigSectionDecl {
            name: name.to_string(),
            schema: StringNode::new(schema_json),
        }
    }

    /// Build a minimal `ExtensionMetadata` with just a name + the given
    /// config sections. Other fields use Default-ish values.
    fn meta_with_sections(name: &str, sections: Vec<ConfigSectionDecl>) -> ExtensionMetadata {
        ExtensionMetadata {
            name: StringNode::new(name),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::default(),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::default(),
            config_sections: sections,
            reload_gates: vec![],
        }
    }

    #[test]
    fn section_key_formats_as_documented() {
        assert_eq!(section_key("my-ext", "editor"), "extension.my-ext.editor");
    }

    #[test]
    fn extract_section_returns_value_when_present() {
        let f = Figment::new().merge(Toml::string(
            r#"
            [extension.my-ext.editor]
            tab-size = 4
            "#,
        ));
        let sec = extract_section(&f, "my-ext", "editor").unwrap();
        assert!(sec.is_some(), "section should be loaded from toml");
        let v = sec.unwrap();
        assert_eq!(v.get("tab-size").and_then(|n| n.as_u64()), Some(4));
    }

    #[test]
    fn extract_section_returns_none_when_absent() {
        let f = Figment::new();
        let sec = extract_section(&f, "my-ext", "nonexistent").unwrap();
        assert!(sec.is_none());
    }

    #[test]
    fn extract_section_handles_unrelated_ext_sections_side_by_side() {
        // Multiple extensions + sections in the same figment chain don't
        // bleed into each other's key-paths.
        let f = Figment::new().merge(Toml::string(
            r#"
            [extension.ext-a.editor]
            tab-size = 2

            [extension.ext-b.editor]
            tab-size = 8
            "#,
        ));
        let a = extract_section(&f, "ext-a", "editor").unwrap().unwrap();
        let b = extract_section(&f, "ext-b", "editor").unwrap().unwrap();
        assert_eq!(a.get("tab-size").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(b.get("tab-size").and_then(|v| v.as_u64()), Some(8));
    }

    #[test]
    fn validate_skips_optional_missing_sections() {
        let f = Figment::new();
        let sections = vec![sample_decl("editor", r#"{"type":"object"}"#)];
        assert!(validate_ext_sections(&f, "my-ext", &sections).is_ok());
    }

    #[test]
    fn validate_fails_on_missing_required_section() {
        let f = Figment::new();
        let sections = vec![sample_decl(
            "keybindings",
            r#"{"type":"object","required":true}"#,
        )];
        let err = validate_ext_sections(&f, "my-ext", &sections).unwrap_err();
        match err {
            ExtConfigError::MissingRequired { ext_name, section } => {
                assert_eq!(ext_name, "my-ext");
                assert_eq!(section, "keybindings");
            }
        }
    }

    #[test]
    fn validate_accepts_required_section_when_present() {
        let f = Figment::new().merge(Toml::string(
            r#"
            [extension.my-ext.keybindings]
            quit = "q"
            "#,
        ));
        let sections = vec![sample_decl(
            "keybindings",
            r#"{"type":"object","required":true}"#,
        )];
        assert!(validate_ext_sections(&f, "my-ext", &sections).is_ok());
    }

    #[test]
    fn validate_all_extensions_stops_at_first_missing() {
        let f = Figment::new().merge(Toml::string(
            r#"
            [extension.ext-a.keybindings]
            quit = "q"
            "#,
        ));
        let manifests = vec![
            (
                "ext-a".to_string(),
                meta_with_sections(
                    "ext-a",
                    vec![sample_decl(
                        "keybindings",
                        r#"{"type":"object","required":true}"#,
                    )],
                ),
            ),
            (
                "ext-b".to_string(),
                meta_with_sections(
                    "ext-b",
                    vec![sample_decl(
                        "core",
                        r#"{"type":"object","required":true}"#,
                    )],
                ),
            ),
        ];
        let err = validate_all_extensions(&f, &manifests).unwrap_err();
        match err {
            ExtConfigError::MissingRequired { ext_name, section } => {
                assert_eq!(ext_name, "ext-b");
                assert_eq!(section, "core");
            }
        }
    }

    #[test]
    fn validate_all_extensions_passes_when_every_required_section_satisfied() {
        let f = Figment::new().merge(Toml::string(
            r#"
            [extension.ext-a.keybindings]
            quit = "q"

            [extension.ext-b.core]
            tag = "v1"
            "#,
        ));
        let manifests = vec![
            (
                "ext-a".to_string(),
                meta_with_sections(
                    "ext-a",
                    vec![sample_decl(
                        "keybindings",
                        r#"{"type":"object","required":true}"#,
                    )],
                ),
            ),
            (
                "ext-b".to_string(),
                meta_with_sections(
                    "ext-b",
                    vec![sample_decl(
                        "core",
                        r#"{"type":"object","required":true}"#,
                    )],
                ),
            ),
        ];
        assert!(validate_all_extensions(&f, &manifests).is_ok());
    }

    #[test]
    fn malformed_schema_is_treated_as_optional() {
        // Schema-driven required enforcement is best-effort; a schema
        // that isn't valid JSON falls through to "optional".
        let f = Figment::new();
        let sections = vec![sample_decl("editor", "this is not json")];
        assert!(validate_ext_sections(&f, "my-ext", &sections).is_ok());
    }

    #[test]
    fn named_error_carries_ext_and_section() {
        // Pin the Display shape so `ark doctor` / boot logs can match on
        // the surface text.
        let err = ExtConfigError::MissingRequired {
            ext_name: "xyz".to_string(),
            section: "abc".to_string(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("xyz"), "display must mention ext name: {msg}");
        assert!(msg.contains("abc"), "display must mention section: {msg}");
    }
}
