//! Validate user-supplied `config { … }` blocks against an extension's
//! declared `ConfigSchema` (cavekit-scene R10, T-10.5).
//!
//! After [`crate::use_resolution::resolve_use`] returns a
//! `ResolvedUse`, the scene compiler invokes [`validate_ext_config`]
//! to check that the user's config matches the schema the extension
//! advertised in its `ExtensionMetadata`. Two failure modes:
//!
//! * Unknown key → [`SceneError::PluginUnknownConfigKey`] (code
//!   `plugin/unknown-config-key`). Carries a "did you mean …?"
//!   suggestion sourced from [`crate::suggest::suggest_similar`]
//!   (T-1.3) when a typo-adjacent schema key exists; otherwise the
//!   diagnostic just lists the available keys for the extension.
//!
//! * Type mismatch → [`SceneError::ExtBadConfig`] (code
//!   `ext/bad-config`). Carries a human-readable mismatch message.
//!
//! Required fields with no user-supplied value also surface as
//! `ext/bad-config` ("required but missing"), so authors get one
//! diagnostic family per config-misuse mode.
//!
//! # Why a synthetic input shape (`UserConfigEntry`)?
//!
//! The scene AST currently models `UseNode::config` as
//! [`crate::ast::OpaqueBlock`] with positional args only — the parser
//! doesn't yet expose the per-key children of the `config { }` body
//! (TODO marked in `ast.rs`). To keep T-10.5 testable today, this
//! module accepts a `&[UserConfigEntry]` slice that callers
//! (eventually a re-walking pass over the raw KDL document) populate.
//! The validation logic + error surface are complete; only the
//! AST → entries pump is deferred. Once the AST grows real
//! key-typed children the public surface here stays the same — only
//! the populate-step changes.
//!
//! # Type-name vocabulary
//!
//! `ConfigField::type_name` uses the v0.1 alphabet enumerated in
//! `ark-ext-metadata-types`:
//!
//! | Tag        | Accepts                                                     |
//! |------------|-------------------------------------------------------------|
//! | `string`   | Any UTF-8 string (no validation beyond UTF-8).              |
//! | `int`      | Decimal `i64` parseable by `str::parse::<i64>`.             |
//! | `bool`     | `true` / `false` (case-insensitive).                        |
//! | `path`     | Any non-empty string (path-existence check is runtime).     |
//! | `url`      | Any string with a `<scheme>://` prefix.                     |
//! | `duration` | `humantime`-compatible (e.g. `30s`, `1m`, `2h`). Until the  |
//! |            | `humantime` dep is wired, accepts the same numeric+suffix   |
//! |            | grammar via a small parser.                                 |
//!
//! Unknown / mistyped tags surface as `ext/bad-config` with a
//! "schema declares an unsupported type" message — the manifest
//! itself is broken, not the user's config.

use ark_ext_metadata_types::{ConfigField, ConfigSchema};

use crate::error::SceneError;
use crate::suggest::suggest_similar;

/// One key/value pair from a user's `config { }` block.
///
/// Held as raw strings — the scene compile pipeline only knows how
/// to read string scalars out of KDL nodes for v0.1; coercion to the
/// declared type happens in [`validate_ext_config`]. Once the AST
/// grows typed config children, callers can shift to a richer
/// `UserConfigValue` enum and rewrite the populate pass; the public
/// surface here stays stable because the type checker still wants
/// the raw string for error reporting.
#[derive(Debug, Clone)]
pub struct UserConfigEntry {
    /// Key name as it appeared in the user's KDL.
    pub key: String,
    /// Value verbatim. Numeric, boolean, etc. are all stringly-typed
    /// at this layer — the validator coerces per the schema's
    /// `type_name`.
    pub raw_value: String,
}

impl UserConfigEntry {
    /// Convenience constructor for tests / call-sites that have a
    /// `(key, value)` tuple in hand.
    pub fn new(key: impl Into<String>, raw_value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            raw_value: raw_value.into(),
        }
    }
}

/// Validated config record returned to the scene compiler.
///
/// Fields are returned in the schema's declared order — not the
/// user's source order — so downstream consumers (intent dispatch,
/// extension `load`) see a stable shape regardless of how the user
/// wrote the block. Defaults from the schema fill in optional
/// fields the user omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedConfigEntry {
    /// Field name (matches `ConfigField::name`).
    pub key: String,
    /// Coerced value, rendered as a string per the v0.1 wire format
    /// (zellij's plugin `load` API takes `BTreeMap<String, String>`).
    /// Numeric / boolean fields are normalised: `"7"`, `"true"`.
    pub value: String,
}

/// Validate a user's `config { }` block against the extension's
/// declared schema.
///
/// `ext_name` is used for diagnostic messages only — pass the name
/// from the `use "<name>"` decl. `schema` is the
/// `ExtensionMetadata::config` field returned by T-10.4. `entries`
/// is the user's flat key/value list (see module docs for why this
/// is synthetic for v0.1).
///
/// Returns `Ok(Vec<ValidatedConfigEntry>)` — one entry per *schema*
/// field, in declared order, with user-supplied values coerced and
/// schema defaults filling in the rest. Returns `Err` on the first
/// problem (fail-fast; matches T-7.6's
/// [`crate::config_schema::validate_config_block`]).
#[allow(clippy::result_large_err)] // SceneError carries diagnostic surface.
pub fn validate_ext_config(
    ext_name: &str,
    schema: &ConfigSchema,
    entries: &[UserConfigEntry],
) -> Result<Vec<ValidatedConfigEntry>, SceneError> {
    let known_keys: Vec<&str> =
        schema.fields.iter().map(|f| f.name.as_str()).collect();

    // 1. Unknown-key check + per-entry type coercion. We collect
    //    coerced values keyed by name in source order; later we
    //    re-emit in schema order.
    use std::collections::BTreeMap;
    let mut user_values: BTreeMap<String, String> = BTreeMap::new();
    for entry in entries {
        let Some(field) = schema.fields.iter().find(|f| f.name == entry.key) else {
            // Unknown key — emit with a did-you-mean.
            let suggestion = suggest_similar(&entry.key, &known_keys)
                .into_iter()
                .next();
            // Reuse the existing `PluginUnknownConfigKey` variant
            // (R12 already enumerates `plugin/unknown-config-key` for
            // the same conceptual error from the plugin path; an
            // extension cartridge is just a wasm-delivered plugin).
            // The structured fields don't carry a suggestion field
            // today; the suggestion is folded into the rendered
            // message via a post-format note. Future work can add a
            // `suggestion: Option<String>` field once R12 grows a
            // dedicated `ext/unknown-config-key` code.
            let key = match suggestion {
                Some(s) => format!("{} (did you mean `{s}`?)", entry.key),
                None => entry.key.clone(),
            };
            return Err(SceneError::PluginUnknownConfigKey {
                plugin: ext_name.to_string(),
                key,
            });
        };
        let coerced = coerce_value(field, &entry.raw_value).map_err(|message| {
            SceneError::ExtBadConfig {
                ext: ext_name.to_string(),
                key: entry.key.clone(),
                message,
            }
        })?;
        user_values.insert(entry.key.clone(), coerced);
    }

    // 2. Walk the schema in declared order. Required fields without a
    //    user value error out; optional fields fall back to the
    //    schema default if any.
    let mut out = Vec::with_capacity(schema.fields.len());
    for field in &schema.fields {
        if let Some(v) = user_values.remove(&field.name) {
            out.push(ValidatedConfigEntry {
                key: field.name.clone(),
                value: v,
            });
            continue;
        }
        if field.required {
            return Err(SceneError::ExtBadConfig {
                ext: ext_name.to_string(),
                key: field.name.clone(),
                message: format!(
                    "required but no value supplied (declared type `{}`)",
                    field.type_name.value
                ),
            });
        }
        if let Some(default) = &field.default {
            // Re-validate the default against the declared type so
            // a malformed default in the manifest surfaces as
            // ext/bad-config (manifest is wrong) rather than
            // silently passing through.
            let coerced = coerce_value(field, &default.value).map_err(|message| {
                SceneError::ExtBadConfig {
                    ext: ext_name.to_string(),
                    key: field.name.clone(),
                    message: format!("default value invalid: {message}"),
                }
            })?;
            out.push(ValidatedConfigEntry {
                key: field.name.clone(),
                value: coerced,
            });
        }
        // No default + not required = field omitted from output.
    }

    Ok(out)
}

/// Coerce a raw string into the canonical wire form for a declared
/// field type. Returns the coerced value on success, an
/// already-formatted error message on type mismatch.
///
/// This is a v0.1 string-in / string-out coercer because zellij's
/// plugin `load` API consumes `BTreeMap<String, String>`. Numerics
/// are normalised through `Display` (`"007"` → `"7"`), booleans are
/// case-folded, durations are normalised to seconds suffix.
fn coerce_value(field: &ConfigField, raw: &str) -> Result<String, String> {
    match field.type_name.value.as_str() {
        "string" => Ok(raw.to_string()),
        "path" => {
            // No FS check — that's a runtime concern. Just refuse the
            // empty string, which is never a meaningful path.
            if raw.is_empty() {
                Err("expected non-empty path".to_string())
            } else {
                Ok(raw.to_string())
            }
        }
        "url" => {
            if raw.contains("://") {
                Ok(raw.to_string())
            } else {
                Err(format!(
                    "expected url with `<scheme>://` prefix, got {raw:?}"
                ))
            }
        }
        "int" => raw
            .trim()
            .parse::<i64>()
            .map(|n| n.to_string())
            .map_err(|e| format!("expected int, got {raw:?}: {e}")),
        "bool" => match raw.trim().to_ascii_lowercase().as_str() {
            "true" => Ok("true".to_string()),
            "false" => Ok("false".to_string()),
            other => Err(format!("expected bool (true/false), got {other:?}")),
        },
        "duration" => parse_duration(raw)
            .map(|secs| format!("{secs}s"))
            .map_err(|e| format!("expected duration (e.g. `30s`, `2m`, `1h`): {e}")),
        other => Err(format!(
            "schema declares unsupported type `{other}` for this field; v0.1 accepts string|int|bool|path|url|duration"
        )),
    }
}

/// Parse a duration string of the form `<n><unit>` where `unit ∈
/// {s, ms, m, h, d}`. Returns the value normalised to seconds.
///
/// Hand-rolled to avoid an external `humantime` dep for v0.1 — once
/// more places need duration parsing, swap in `humantime`'s own
/// `parse_duration` which accepts a richer grammar (`1h30m`,
/// `1.5s`, etc.).
fn parse_duration(raw: &str) -> Result<u64, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty duration".to_string());
    }
    // Find the boundary between digits and the unit suffix.
    let split = raw
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("missing unit suffix in {raw:?}"))?;
    let (num, unit) = raw.split_at(split);
    let n: u64 = num
        .parse()
        .map_err(|e| format!("invalid integer `{num}`: {e}"))?;
    let secs = match unit.trim() {
        "s" => n,
        "ms" => n / 1000,
        "m" => n.saturating_mul(60),
        "h" => n.saturating_mul(3600),
        "d" => n.saturating_mul(86_400),
        other => return Err(format!("unknown unit `{other}`")),
    };
    Ok(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use ark_ext_metadata_types::{ConfigField, ConfigSchema, StringNode};

    /// Build a `ConfigSchema` with the supplied fields. Convenience
    /// because the upstream constructor is verbose for tests.
    fn schema(fields: Vec<ConfigField>) -> ConfigSchema {
        ConfigSchema { fields }
    }

    fn field(
        name: &str,
        ty: &str,
        required: bool,
        default: Option<&str>,
    ) -> ConfigField {
        ConfigField {
            name: name.to_string(),
            type_name: StringNode::new(ty),
            required,
            default: default.map(StringNode::new),
        }
    }

    #[test]
    fn accepts_valid_config_with_all_supported_types() {
        let s = schema(vec![
            field("greeting", "string", true, None),
            field("retries", "int", true, None),
            field("verbose", "bool", true, None),
            field("home", "path", true, None),
            field("upstream", "url", true, None),
            field("timeout", "duration", true, None),
        ]);
        let entries = vec![
            UserConfigEntry::new("greeting", "hi"),
            UserConfigEntry::new("retries", "7"),
            UserConfigEntry::new("verbose", "True"),
            UserConfigEntry::new("home", "/home/x"),
            UserConfigEntry::new("upstream", "https://example.com"),
            UserConfigEntry::new("timeout", "30s"),
        ];
        let out = validate_ext_config("ext", &s, &entries).expect("valid");
        // Order is schema order regardless of source.
        assert_eq!(out.len(), 6);
        assert_eq!(out[0], ValidatedConfigEntry { key: "greeting".into(), value: "hi".into() });
        assert_eq!(out[1], ValidatedConfigEntry { key: "retries".into(), value: "7".into() });
        assert_eq!(out[2], ValidatedConfigEntry { key: "verbose".into(), value: "true".into() });
        assert_eq!(out[3], ValidatedConfigEntry { key: "home".into(), value: "/home/x".into() });
        assert_eq!(out[4], ValidatedConfigEntry { key: "upstream".into(), value: "https://example.com".into() });
        assert_eq!(out[5], ValidatedConfigEntry { key: "timeout".into(), value: "30s".into() });
    }

    #[test]
    fn unknown_key_surfaces_did_you_mean_suggestion() {
        let s = schema(vec![
            field("greeting", "string", false, None),
            field("retries", "int", false, None),
        ]);
        let entries = vec![UserConfigEntry::new("gretng", "hi")];
        let err = validate_ext_config("ext", &s, &entries)
            .expect_err("typo must error");
        assert_eq!(err.code_enum(), ErrorCode::PluginUnknownConfigKey);
        match err {
            SceneError::PluginUnknownConfigKey { plugin, key } => {
                assert_eq!(plugin, "ext");
                assert!(
                    key.contains("greeting"),
                    "expected suggestion in key field, got {key:?}"
                );
                assert!(key.contains("did you mean"));
            }
            other => panic!("expected PluginUnknownConfigKey, got {other:?}"),
        }
    }

    #[test]
    fn unknown_key_without_close_match_renders_plain_key() {
        let s = schema(vec![field("greeting", "string", false, None)]);
        let entries = vec![UserConfigEntry::new("xyzzy", "hi")];
        let err = validate_ext_config("ext", &s, &entries)
            .expect_err("unknown key must error");
        match err {
            SceneError::PluginUnknownConfigKey { key, .. } => {
                // No suggestion fold-in.
                assert_eq!(key, "xyzzy");
            }
            other => panic!("expected PluginUnknownConfigKey, got {other:?}"),
        }
    }

    #[test]
    fn bad_int_value_surfaces_ext_bad_config() {
        let s = schema(vec![field("retries", "int", true, None)]);
        let entries = vec![UserConfigEntry::new("retries", "not-an-int")];
        let err = validate_ext_config("ext", &s, &entries)
            .expect_err("non-int must error");
        assert_eq!(err.code_enum(), ErrorCode::ExtBadConfig);
        match err {
            SceneError::ExtBadConfig { ext, key, message } => {
                assert_eq!(ext, "ext");
                assert_eq!(key, "retries");
                assert!(message.contains("expected int"));
            }
            other => panic!("expected ExtBadConfig, got {other:?}"),
        }
    }

    #[test]
    fn bad_bool_value_surfaces_ext_bad_config() {
        let s = schema(vec![field("verbose", "bool", true, None)]);
        let entries = vec![UserConfigEntry::new("verbose", "yes")];
        let err = validate_ext_config("ext", &s, &entries)
            .expect_err("non-bool must error");
        match err {
            SceneError::ExtBadConfig { message, .. } => {
                assert!(message.contains("bool"));
            }
            other => panic!("expected ExtBadConfig, got {other:?}"),
        }
    }

    #[test]
    fn missing_required_field_surfaces_ext_bad_config() {
        let s = schema(vec![field("greeting", "string", true, None)]);
        let err = validate_ext_config("ext", &s, &[])
            .expect_err("missing required must error");
        match err {
            SceneError::ExtBadConfig { key, message, .. } => {
                assert_eq!(key, "greeting");
                assert!(message.contains("required"));
            }
            other => panic!("expected ExtBadConfig, got {other:?}"),
        }
    }

    #[test]
    fn optional_field_with_default_fills_in() {
        let s = schema(vec![
            field("greeting", "string", false, Some("hello")),
            field("retries", "int", false, Some("3")),
        ]);
        // User supplies neither.
        let out = validate_ext_config("ext", &s, &[]).expect("defaults fill in");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].value, "hello");
        assert_eq!(out[1].value, "3");
    }

    #[test]
    fn optional_field_without_default_omitted_when_user_silent() {
        let s = schema(vec![field("greeting", "string", false, None)]);
        let out = validate_ext_config("ext", &s, &[]).expect("no default → omit");
        assert!(out.is_empty());
    }

    #[test]
    fn schema_with_unsupported_type_surfaces_ext_bad_config() {
        let s = schema(vec![field("weird", "rocket", true, None)]);
        let entries = vec![UserConfigEntry::new("weird", "value")];
        let err = validate_ext_config("ext", &s, &entries)
            .expect_err("unsupported type tag must error");
        match err {
            SceneError::ExtBadConfig { message, .. } => {
                assert!(message.contains("unsupported type"));
            }
            other => panic!("expected ExtBadConfig, got {other:?}"),
        }
    }

    #[test]
    fn empty_schema_with_no_user_entries_is_ok() {
        let out = validate_ext_config("ext", &ConfigSchema::default(), &[])
            .expect("empty/empty");
        assert!(out.is_empty());
    }

    #[test]
    fn empty_schema_with_user_entry_surfaces_unknown_key() {
        let entries = vec![UserConfigEntry::new("anything", "value")];
        let err = validate_ext_config("ext", &ConfigSchema::default(), &entries)
            .expect_err("any key must surface unknown");
        assert_eq!(err.code_enum(), ErrorCode::PluginUnknownConfigKey);
    }

    #[test]
    fn url_must_have_scheme_prefix() {
        let s = schema(vec![field("upstream", "url", true, None)]);
        let entries = vec![UserConfigEntry::new("upstream", "example.com")];
        let err = validate_ext_config("ext", &s, &entries)
            .expect_err("missing scheme must error");
        match err {
            SceneError::ExtBadConfig { message, .. } => {
                assert!(message.contains("url"));
            }
            other => panic!("expected ExtBadConfig, got {other:?}"),
        }
    }

    #[test]
    fn duration_normalised_to_seconds_suffix() {
        let s = schema(vec![
            field("a", "duration", true, None),
            field("b", "duration", true, None),
            field("c", "duration", true, None),
        ]);
        let entries = vec![
            UserConfigEntry::new("a", "30s"),
            UserConfigEntry::new("b", "2m"),
            UserConfigEntry::new("c", "1h"),
        ];
        let out = validate_ext_config("ext", &s, &entries).expect("durations valid");
        assert_eq!(out[0].value, "30s");
        assert_eq!(out[1].value, "120s");
        assert_eq!(out[2].value, "3600s");
    }

    #[test]
    fn duration_rejects_missing_unit() {
        let s = schema(vec![field("t", "duration", true, None)]);
        let entries = vec![UserConfigEntry::new("t", "30")];
        let err = validate_ext_config("ext", &s, &entries)
            .expect_err("missing unit must error");
        match err {
            SceneError::ExtBadConfig { message, .. } => {
                assert!(message.to_lowercase().contains("duration"));
            }
            other => panic!("expected ExtBadConfig, got {other:?}"),
        }
    }

    #[test]
    fn malformed_default_in_manifest_surfaces_ext_bad_config() {
        // Schema declares an int field with default `"abc"` — that's
        // a manifest-author bug. Validation must surface, not crash.
        let s = schema(vec![field("retries", "int", false, Some("abc"))]);
        let err = validate_ext_config("ext", &s, &[])
            .expect_err("malformed default must error");
        match err {
            SceneError::ExtBadConfig { key, message, .. } => {
                assert_eq!(key, "retries");
                assert!(message.contains("default"));
            }
            other => panic!("expected ExtBadConfig, got {other:?}"),
        }
    }
}
