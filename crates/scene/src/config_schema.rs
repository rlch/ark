//! Shipped-plugin Config schema registry — T-7.6.
//!
//! Every `plugin "<name>" { config { … } }` block is validated against
//! an in-process schema registered under the plugin name. For the
//! v0.1 shipped cartridge set (picker, status, ark-bus) the schemas
//! are defined here as `#[derive(Facet)]` structs — facet's SHAPE
//! reflection surfaces the field set + types without touching the
//! plugin's wasm binary. Unknown keys in the user's `config { }`
//! block surface as `plugin/unknown-config-key`; type mismatches on
//! known keys surface as `plugin/bad-config`.
//!
//! # Why empty Config structs?
//!
//! The three shipped plugins currently ignore their zellij `BTreeMap<
//! String, String>` configuration map (see
//! `crates/plugins/{picker,status,ark-bus}/src/lib.rs`'s
//! `ZellijPlugin::load` impls — every one takes `_configuration`). So
//! the v0.1 schemas are intentionally empty: the validation path
//! rejects *any* key in the user's `config { }` block, locking down
//! the surface until plugins grow real options. Follow-up tiers flesh
//! out per-plugin fields as behaviour settles.
//!
//! # Wasm cartridge schemas (T-10.2 / T-10.5)
//!
//! For wasm cartridges sourced from `file:` / `url:` URIs, the Config
//! struct lives inside the cartridge itself and surfaces via the
//! `ark.metadata` custom section (T-10.2). Those schemas arrive in
//! v0.3; until then unknown plugins (no entry in the registry) are
//! passed through untyped. This is the "degrade to untyped
//! pass-through" escape hatch spelled out in T-7.6's acceptance
//! criteria. See the per-function docs for the exact behaviour.
//!
//! # Serialisation to zellij's `BTreeMap<String, String>`
//!
//! Zellij's plugin load API accepts a `BTreeMap<String, String>`
//! (see `ZellijPlugin::load`). Typed Config values are serialised to
//! that flat-string form at mount time via
//! [`serialize_to_flat_strings`]. facet-reflect's fallible walkers
//! aren't used here because the shipped Configs are empty — when
//! fields arrive, the serialiser grows per-type branches for each
//! field type (`String`, `u32`, `bool`, etc.) without leaving the
//! flat-string contract.

use std::collections::BTreeMap;

use facet::Facet;

use crate::ast::OpaqueBlock;
use crate::error::SceneError;

/// Picker plugin Config (v0.1 shipped).
///
/// The picker currently ignores its zellij `BTreeMap` configuration
/// at `load` time (see the `ZellijPlugin::load` impl in
/// `crates/plugins/picker/src/lib.rs`), so the v0.1 schema is empty.
/// Follow-up tiers grow per-field options — at which point each new
/// field's `#[facet(...)]` doc-comment surfaces through SHAPE
/// reflection to LSP hover + `ark scene check` output.
#[derive(Facet, Debug, Default)]
#[facet(traits(Default))]
pub struct PickerConfig {}

/// Status-bar plugin Config (v0.1 shipped).
///
/// Same rationale as [`PickerConfig`]: the status plugin's
/// `ZellijPlugin::load` impl currently ignores its configuration
/// map. Empty schema today; grow per field as settings land.
#[derive(Facet, Debug, Default)]
#[facet(traits(Default))]
pub struct StatusConfig {}

/// ark-bus plugin Config (v0.1 shipped).
///
/// Same rationale as [`PickerConfig`]. ark-bus already accepts its
/// rebind surface over zellij's `rebind_keys` channel at runtime — no
/// compile-time Config fields are needed until the bus grows its
/// own per-instance tunables (e.g. `max_inflight` / `backpressure`).
#[derive(Facet, Debug, Default)]
#[facet(traits(Default))]
pub struct ArkBusConfig {}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Name of the known shipped schemas. Order is the canonical list
/// rendered in help text; keep it stable.
pub const SHIPPED_PLUGIN_NAMES: &[&str] = &["picker", "status", "ark-bus"];

/// Immutable schema record held in the registry.
///
/// Stores the list of accepted key names. Value-type validation
/// lives behind the `validate` callback so per-schema rules (CEL-ish
/// constraints, bounded integers) can extend past "is this key
/// known" without changing the registry surface.
#[derive(Clone)]
pub struct ConfigSchema {
    /// Plugin name this schema validates (e.g. `"picker"`).
    pub plugin: &'static str,
    /// Sorted list of keys the schema accepts. Empty list means the
    /// plugin takes NO config at all — any user key surfaces as
    /// `plugin/unknown-config-key`.
    pub keys: &'static [&'static str],
}

impl std::fmt::Debug for ConfigSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigSchema")
            .field("plugin", &self.plugin)
            .field("keys", &self.keys)
            .finish()
    }
}

/// Registry of in-proc Config schemas keyed by plugin name.
///
/// Built once at scene compile time via [`shipped_schemas`]; extended
/// in later tiers when extensions / wasm cartridges contribute their
/// own schemas.
#[derive(Debug, Clone, Default)]
pub struct ConfigSchemaRegistry {
    /// BTreeMap so iteration order is deterministic — tests and
    /// `ark scene check` output both benefit.
    schemas: BTreeMap<String, ConfigSchema>,
}

impl ConfigSchemaRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a schema under its plugin name. Overwrites any existing
    /// entry — extensions / wasm cartridges whose schemas arrive later
    /// can replace a shipped default.
    pub fn register(&mut self, schema: ConfigSchema) {
        self.schemas.insert(schema.plugin.to_string(), schema);
    }

    /// Fetch the schema for `plugin` if one is registered.
    pub fn get(&self, plugin: &str) -> Option<&ConfigSchema> {
        self.schemas.get(plugin)
    }

    /// Whether the registry knows about `plugin`.
    pub fn has(&self, plugin: &str) -> bool {
        self.schemas.contains_key(plugin)
    }

    /// Iterate every registered `(plugin_name, schema)` pair in
    /// BTreeMap order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ConfigSchema)> {
        self.schemas.iter()
    }

    /// Number of registered schemas.
    pub fn len(&self) -> usize {
        self.schemas.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.schemas.is_empty()
    }
}

/// Build a [`ConfigSchemaRegistry`] populated with the v0.1 shipped
/// plugin schemas (picker, status, ark-bus). Entry point the scene
/// compile pipeline calls at the start of its validation pass.
pub fn shipped_schemas() -> ConfigSchemaRegistry {
    let mut reg = ConfigSchemaRegistry::new();
    reg.register(ConfigSchema {
        plugin: "picker",
        keys: &[],
    });
    reg.register(ConfigSchema {
        plugin: "status",
        keys: &[],
    });
    reg.register(ConfigSchema {
        plugin: "ark-bus",
        keys: &[],
    });
    reg
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a plugin `config { }` block against the shipped schema
/// registered for `plugin_name`.
///
/// Returns `Ok(())` in three cases:
///
/// 1. The plugin is not registered — per the module-level doc, unknown
///    plugins degrade to untyped pass-through until T-10.5 lands the
///    wasm-derived schema path.
/// 2. The registered schema accepts an empty key set *and* the user's
///    block is empty.
/// 3. Every declared key in the user's block is present in the
///    schema's `keys` list.
///
/// Returns [`SceneError::PluginUnknownConfigKey`] for the first
/// unrecognised key discovered (validation is fail-fast on unknown
/// keys to keep the diagnostic surface small); future tiers may grow
/// a `collect errors` variant once value-type checks arrive.
///
/// `config_block` is the typed [`OpaqueBlock`] wrapping the raw
/// `config { }` body. For v0.1 the block stores only its positional
/// args; its children come through the AST's richer KDL representation
/// that follow-up tiers expose. We accept `Option<&OpaqueBlock>` for
/// convenience at call sites — `None` means "user provided no config
/// block", which is always valid regardless of schema.
pub fn validate_config_block(
    registry: &ConfigSchemaRegistry,
    plugin_name: &str,
    config_block: Option<&OpaqueBlock>,
    declared_keys: &[String],
) -> Result<(), SceneError> {
    // No config provided — nothing to validate.
    if config_block.is_none() || declared_keys.is_empty() {
        return Ok(());
    }
    let Some(schema) = registry.get(plugin_name) else {
        // Unknown plugin — degrade to untyped pass-through per module
        // docs. Tracing only; no error surface.
        tracing::debug!(
            target = "scene::config",
            plugin = plugin_name,
            "no config schema registered; passing config block through untyped",
        );
        return Ok(());
    };

    // First unrecognised key wins — fail-fast on unknowns.
    for key in declared_keys {
        if !schema.keys.contains(&key.as_str()) {
            return Err(SceneError::PluginUnknownConfigKey {
                plugin: plugin_name.to_string(),
                key: key.clone(),
            });
        }
    }
    Ok(())
}

/// Serialise a typed Config value to the `BTreeMap<String, String>`
/// shape zellij's plugin `load` API consumes (R10 / T-7.6).
///
/// For v0.1 every shipped schema is EMPTY, so the output is always
/// an empty map. The signature accepts the registry + plugin name so
/// call sites can thread through the same pair they used for
/// validation; later tiers grow per-field extractors here.
///
/// Future work:
///
/// * When a schema grows fields, each `#[facet(kdl::property)]` /
///   `#[facet(kdl::argument)]` on the Config struct maps to one
///   `BTreeMap` entry with the key being the facet field name (minus
///   the `#[facet(rename = "…")]` override, if any) and the value
///   being the `Display` rendering of the field value.
/// * Boolean fields render as `"true"` / `"false"`. Numeric fields use
///   `Display`. Nested structs are flattened via dotted keys
///   (`outer.inner=value`).
pub fn serialize_to_flat_strings(
    _registry: &ConfigSchemaRegistry,
    _plugin_name: &str,
) -> BTreeMap<String, String> {
    // Empty for v0.1 — see function docs.
    BTreeMap::new()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn block() -> OpaqueBlock {
        OpaqueBlock::default()
    }

    #[test]
    fn shipped_schemas_registers_three_plugins() {
        let reg = shipped_schemas();
        assert_eq!(reg.len(), 3);
        assert!(reg.has("picker"));
        assert!(reg.has("status"));
        assert!(reg.has("ark-bus"));
    }

    #[test]
    fn shipped_schemas_are_all_empty() {
        let reg = shipped_schemas();
        for (_name, schema) in reg.iter() {
            assert!(
                schema.keys.is_empty(),
                "v0.1 shipped schemas must be empty until plugins grow options"
            );
        }
    }

    #[test]
    fn validate_accepts_missing_config_block() {
        let reg = shipped_schemas();
        let blk = block();
        assert!(validate_config_block(&reg, "picker", None, &[]).is_ok());
        assert!(validate_config_block(&reg, "picker", Some(&blk), &[]).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_key_on_empty_schema() {
        let reg = shipped_schemas();
        let blk = block();
        let err = validate_config_block(
            &reg,
            "picker",
            Some(&blk),
            &["unrecognised_key".to_string()],
        )
        .expect_err("unknown key must fail");
        assert_eq!(err.code_enum(), ErrorCode::PluginUnknownConfigKey);
        match err {
            SceneError::PluginUnknownConfigKey { plugin, key } => {
                assert_eq!(plugin, "picker");
                assert_eq!(key, "unrecognised_key");
            }
            other => panic!("expected PluginUnknownConfigKey, got {other:?}"),
        }
    }

    #[test]
    fn validate_accepts_unknown_plugin_as_pass_through() {
        // No schema registered for "third-party" — validation must
        // NOT surface any error (pass-through contract).
        let reg = ConfigSchemaRegistry::new();
        let blk = block();
        assert!(
            validate_config_block(
                &reg,
                "third-party",
                Some(&blk),
                &["any_key".to_string()],
            )
            .is_ok(),
            "unknown plugin must degrade to pass-through",
        );
    }

    #[test]
    fn validate_accepts_schema_with_known_key() {
        let mut reg = ConfigSchemaRegistry::new();
        reg.register(ConfigSchema {
            plugin: "my-plugin",
            keys: &["max_items", "theme"],
        });
        let blk = block();
        assert!(
            validate_config_block(
                &reg,
                "my-plugin",
                Some(&blk),
                &["max_items".to_string()],
            )
            .is_ok(),
        );
    }

    #[test]
    fn validate_rejects_mixed_known_and_unknown_keys() {
        let mut reg = ConfigSchemaRegistry::new();
        reg.register(ConfigSchema {
            plugin: "my-plugin",
            keys: &["max_items"],
        });
        let blk = block();
        let err = validate_config_block(
            &reg,
            "my-plugin",
            Some(&blk),
            &["max_items".to_string(), "bogus".to_string()],
        )
        .expect_err("bogus key must fail");
        assert_eq!(err.code_enum(), ErrorCode::PluginUnknownConfigKey);
    }

    #[test]
    fn serialize_shipped_configs_is_empty_map() {
        let reg = shipped_schemas();
        for name in SHIPPED_PLUGIN_NAMES {
            let map = serialize_to_flat_strings(&reg, name);
            assert!(
                map.is_empty(),
                "{name} serialises to empty map under v0.1",
            );
        }
    }

    #[test]
    fn config_schema_registry_overwrites_on_reregister() {
        let mut reg = ConfigSchemaRegistry::new();
        reg.register(ConfigSchema {
            plugin: "foo",
            keys: &[],
        });
        reg.register(ConfigSchema {
            plugin: "foo",
            keys: &["bar"],
        });
        assert_eq!(reg.get("foo").unwrap().keys, &["bar"]);
    }

    #[test]
    fn bad_config_error_renders_with_correct_code() {
        let err = SceneError::PluginBadConfig {
            plugin: "picker".to_string(),
            message: "field foo must be a positive integer".to_string(),
        };
        assert_eq!(err.code_enum(), ErrorCode::PluginBadConfig);
    }

    #[test]
    fn picker_status_bus_facet_schemas_compile() {
        // These structs exist solely to let facet SHAPE reflection
        // surface the shipped plugin configs. Constructing one proves
        // the derive + Default impls stay healthy across refactors.
        let _p = PickerConfig::default();
        let _s = StatusConfig::default();
        let _b = ArkBusConfig::default();
    }
}
