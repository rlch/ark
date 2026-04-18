//! Extension config ownership validation (T-096).
//!
//! When a scene file writes `use "<ext>" config { … }`, the config block
//! is raw KDL text captured at parse time. This module validates that
//! block against the extension's declared [`ConfigSchema`] at
//! `ark scene check` time, before the extension ever sees the values.
//!
//! Validation checks, in order:
//! 1. Unknown keys — every key in the config block must exist in the schema.
//! 2. Missing required fields — every `required: true` schema field must
//!    appear in the config block.
//! 3. Type checking — each present key's KDL value type must match the
//!    schema's `field_type` (`"string"`, `"bool"`, `"int"`, `"path"`,
//!    `"url"`, `"duration"`).

use ark_ext_metadata_types::ConfigSchema;
use miette::{NamedSource, SourceSpan};

use crate::error::SceneError;

/// Validate a `use` node's config block against the extension's declared schema.
///
/// `config_block` is raw KDL text (the interior of `config { … }`).
/// `schema` is the extension's declared [`ConfigSchema`].
/// `ext_name` identifies the extension for diagnostic messages.
///
/// Returns `Ok(())` when the config is valid, or the first
/// [`SceneError::ExtBadConfig`] encountered.
pub fn validate_config(
    config_block: &str,
    schema: &ConfigSchema,
    ext_name: &str,
) -> Result<(), SceneError> {
    // Parse the raw config block as a KDL document.
    let doc: kdl::KdlDocument =
        config_block
            .parse()
            .map_err(|e: kdl::KdlError| SceneError::ExtBadConfig {
                ext: ext_name.to_string(),
                message: format!("config block is not valid KDL: {e}"),
                src: NamedSource::new(format!("{ext_name} config"), config_block.to_string()),
                span: SourceSpan::from((0, config_block.len())),
            })?;

    // 1. Unknown keys: every node in the config block must be a known field.
    for node in doc.nodes() {
        let key = node.name().value();
        if !schema.fields.iter().any(|f| f.name == key) {
            let known: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
            let offset = node.name().span().offset();
            let len = node.name().span().len();
            return Err(SceneError::ExtBadConfig {
                ext: ext_name.to_string(),
                message: format!(
                    "unknown config key `{key}`; known keys: [{}]",
                    known.join(", ")
                ),
                src: NamedSource::new(format!("{ext_name} config"), config_block.to_string()),
                span: SourceSpan::from((offset, len)),
            });
        }
    }

    // 2. Missing required fields.
    for field in &schema.fields {
        if field.required && !doc.nodes().iter().any(|n| n.name().value() == field.name) {
            return Err(SceneError::ExtBadConfig {
                ext: ext_name.to_string(),
                message: format!("missing required config key `{}`", field.name),
                src: NamedSource::new(format!("{ext_name} config"), config_block.to_string()),
                span: SourceSpan::from((0, config_block.len())),
            });
        }
    }

    // 3. Type checking: verify each present field's value matches the
    //    declared type.
    for node in doc.nodes() {
        let key = node.name().value();
        let Some(field) = schema.fields.iter().find(|f| f.name == key) else {
            // Already caught in step 1 — unreachable.
            continue;
        };

        // The value is the first entry (argument) on the node.
        let Some(entry) = node.entries().first() else {
            // Node exists but has no value — acceptable only if the field
            // has a default (the extension sees the default). If required
            // with no default, that's a type error.
            if field.required && field.default.is_none() {
                let offset = node.name().span().offset();
                let len = node.name().span().len();
                return Err(SceneError::ExtBadConfig {
                    ext: ext_name.to_string(),
                    message: format!("config key `{key}` requires a value"),
                    src: NamedSource::new(format!("{ext_name} config"), config_block.to_string()),
                    span: SourceSpan::from((offset, len)),
                });
            }
            continue;
        };

        let value = entry.value();
        let expected_type = field.type_name.value.as_str();
        let type_ok = match expected_type {
            "string" | "path" | "url" | "duration" => value.as_string().is_some(),
            "bool" => value.as_bool().is_some(),
            "int" => value.as_integer().is_some(),
            _ => {
                // Unknown type in schema — not our problem here, but
                // we can't validate. Accept it.
                true
            }
        };

        if !type_ok {
            let actual = kdl_value_type_name(value);
            let offset = entry.span().offset();
            let len = entry.span().len();
            return Err(SceneError::ExtBadConfig {
                ext: ext_name.to_string(),
                message: format!("config key `{key}` expects type `{expected_type}`, got {actual}"),
                src: NamedSource::new(format!("{ext_name} config"), config_block.to_string()),
                span: SourceSpan::from((offset, len)),
            });
        }
    }

    Ok(())
}

/// Human-readable name for a KDL value's runtime type.
fn kdl_value_type_name(value: &kdl::KdlValue) -> &'static str {
    if value.as_string().is_some() {
        "string"
    } else if value.as_bool().is_some() {
        "bool"
    } else if value.as_integer().is_some() {
        "integer"
    } else if value.as_float().is_some() {
        "float"
    } else {
        "null"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ext_metadata_types::{ConfigField, ConfigSchema, StringNode};

    /// Helper: build a schema from a list of (name, type, required, default).
    fn schema(fields: &[(&str, &str, bool, Option<&str>)]) -> ConfigSchema {
        ConfigSchema {
            fields: fields
                .iter()
                .map(|(name, ty, required, default)| ConfigField {
                    name: name.to_string(),
                    type_name: StringNode::new(*ty),
                    required: *required,
                    default: default.map(|d| StringNode::new(d)),
                })
                .collect(),
        }
    }

    // ── Valid config passes ────────────────────────────────────────

    #[test]
    fn valid_config_all_fields() {
        let s = schema(&[
            ("host", "string", true, None),
            ("port", "int", true, None),
            ("debug", "bool", false, Some("false")),
        ]);
        let block = r#"host "localhost"
port 8080
debug #true"#;
        assert!(validate_config(block, &s, "demo").is_ok());
    }

    #[test]
    fn valid_config_optional_fields_omitted() {
        let s = schema(&[
            ("host", "string", true, None),
            ("debug", "bool", false, Some("false")),
        ]);
        let block = r#"host "localhost""#;
        assert!(validate_config(block, &s, "demo").is_ok());
    }

    #[test]
    fn empty_config_block_no_required_fields() {
        let s = schema(&[("debug", "bool", false, Some("false"))]);
        let block = "";
        assert!(validate_config(block, &s, "demo").is_ok());
    }

    #[test]
    fn empty_schema_empty_config() {
        let s = ConfigSchema::default();
        let block = "";
        assert!(validate_config(block, &s, "demo").is_ok());
    }

    // ── Unknown key errors ─────────────────────────────────────────

    #[test]
    fn unknown_key_errors() {
        let s = schema(&[("host", "string", true, None)]);
        let block = r#"host "localhost"
bogus "value""#;
        let err = validate_config(block, &s, "demo").unwrap_err();
        match &err {
            SceneError::ExtBadConfig { ext, message, .. } => {
                assert_eq!(ext, "demo");
                assert!(message.contains("unknown config key `bogus`"), "{message}");
            }
            other => panic!("expected ExtBadConfig, got: {other:?}"),
        }
    }

    #[test]
    fn unknown_key_on_empty_schema() {
        let s = ConfigSchema::default();
        let block = r#"surprise "hello""#;
        let err = validate_config(block, &s, "demo").unwrap_err();
        match &err {
            SceneError::ExtBadConfig { message, .. } => {
                assert!(
                    message.contains("unknown config key `surprise`"),
                    "{message}"
                );
            }
            other => panic!("expected ExtBadConfig, got: {other:?}"),
        }
    }

    // ── Missing required field errors ──────────────────────────────

    #[test]
    fn missing_required_field_errors() {
        let s = schema(&[("host", "string", true, None), ("port", "int", true, None)]);
        let block = r#"host "localhost""#;
        let err = validate_config(block, &s, "demo").unwrap_err();
        match &err {
            SceneError::ExtBadConfig { message, .. } => {
                assert!(
                    message.contains("missing required config key `port`"),
                    "{message}"
                );
            }
            other => panic!("expected ExtBadConfig, got: {other:?}"),
        }
    }

    // ── Type mismatch errors ───────────────────────────────────────

    #[test]
    fn type_mismatch_string_vs_int() {
        let s = schema(&[("port", "int", true, None)]);
        let block = r#"port "not-a-number""#;
        let err = validate_config(block, &s, "demo").unwrap_err();
        match &err {
            SceneError::ExtBadConfig { message, .. } => {
                assert!(message.contains("expects type `int`"), "{message}");
                assert!(message.contains("got string"), "{message}");
            }
            other => panic!("expected ExtBadConfig, got: {other:?}"),
        }
    }

    #[test]
    fn type_mismatch_bool_vs_string() {
        let s = schema(&[("debug", "bool", true, None)]);
        let block = r#"debug "yes""#;
        let err = validate_config(block, &s, "demo").unwrap_err();
        match &err {
            SceneError::ExtBadConfig { message, .. } => {
                assert!(message.contains("expects type `bool`"), "{message}");
                assert!(message.contains("got string"), "{message}");
            }
            other => panic!("expected ExtBadConfig, got: {other:?}"),
        }
    }

    #[test]
    fn type_mismatch_int_vs_bool() {
        let s = schema(&[("port", "int", true, None)]);
        let block = r#"port #true"#;
        let err = validate_config(block, &s, "demo").unwrap_err();
        match &err {
            SceneError::ExtBadConfig { message, .. } => {
                assert!(message.contains("expects type `int`"), "{message}");
                assert!(message.contains("got bool"), "{message}");
            }
            other => panic!("expected ExtBadConfig, got: {other:?}"),
        }
    }

    /// String-typed fields also cover `path`, `url`, and `duration`
    /// (all represented as KDL strings on the wire).
    #[test]
    fn path_url_duration_accept_strings() {
        let s = schema(&[
            ("dir", "path", true, None),
            ("endpoint", "url", true, None),
            ("timeout", "duration", true, None),
        ]);
        let block = r#"dir "/tmp"
endpoint "https://example.com"
timeout "30s""#;
        assert!(validate_config(block, &s, "demo").is_ok());
    }

    #[test]
    fn path_rejects_non_string() {
        let s = schema(&[("dir", "path", true, None)]);
        let block = "dir 42";
        let err = validate_config(block, &s, "demo").unwrap_err();
        match &err {
            SceneError::ExtBadConfig { message, .. } => {
                assert!(message.contains("expects type `path`"), "{message}");
            }
            other => panic!("expected ExtBadConfig, got: {other:?}"),
        }
    }
}
