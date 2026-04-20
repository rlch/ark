//! Plugin ABI version constants + errors.
//!
//! T-PP-006 (cavekit-plugin-protocol R14): the host-wide plugin ABI
//! version. A plugin's `ark-meta:v1` custom section (T-PP-017) carries an
//! `ark_abi_version: u32` field; on load, the host computes strict
//! equality against [`ARK_ABI_VERSION`] and refuses any plugin whose
//! declared version is not present in [`SUPPORTED_PLUGIN_ABIS`].
//!
//! v1 is strict-equality: `SUPPORTED_PLUGIN_ABIS = &[1]`. There is no
//! forward-compat window. Adding a new variant to any `#[non_exhaustive]`
//! enum exposed across the ABI boundary (`Target`, `IntentTarget`,
//! `InstallEvent`, …) is a MAJOR bump of the ABI that requires
//! incrementing this constant and adjusting the supported-list.

/// Current plugin ABI version this host builds against.
///
/// Plugins must declare an equal value in their `ark-meta:v1` custom
/// section — no forward compatibility window in v1. Incremented on every
/// MAJOR ABI break (new WIT variant arm, changed function signature,
/// …).
pub const ARK_ABI_VERSION: u32 = 1;

/// Every plugin ABI version this host can load. v1 ships strict
/// equality (`&[1]`); the slice shape is preserved so post-v1 hosts can
/// widen the supported range without an API break.
pub const SUPPORTED_PLUGIN_ABIS: &[u32] = &[1];

/// Errors raised by the host's ABI gate when a plugin's declared ABI
/// does not match the host's.
///
/// Wired into the broader `PluginLoadError` enum in
/// `ark-plugin-protocol` (T-PP-008) via a `From<AbiError>` conversion
/// in Tier 3.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AbiError {
    /// Plugin declares an ABI newer than any the host supports.
    ///
    /// `error[abi/host-too-old]` — user must upgrade ark.
    #[error(
        "error[abi/host-too-old]: plugin {plugin} declares ABI v{plugin_abi}, host supports {host_abi}"
    )]
    HostTooOld {
        plugin: String,
        plugin_abi: u32,
        host_abi: u32,
    },

    /// Plugin declares an ABI older than any the host supports.
    ///
    /// `error[abi/plugin-too-old]` — plugin author must rebuild against
    /// current `ark-plugin-sdk`.
    #[error(
        "error[abi/plugin-too-old]: plugin {plugin} declares ABI v{plugin_abi}, host supports {host_abi}"
    )]
    PluginTooOld {
        plugin: String,
        plugin_abi: u32,
        host_abi: u32,
    },

    /// Plugin's `.wasm` has no `ark-meta:v1` custom section at all, so
    /// the ABI version is unknowable.
    ///
    /// `error[abi/missing-version]` — usually means the plugin was
    /// built without `#[derive(Plugin)]` or the section was stripped.
    #[error("error[abi/missing-version]: plugin {plugin} has no ark-meta:v1 custom section")]
    MissingVersion { plugin: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ark_abi_version_is_one() {
        assert_eq!(ARK_ABI_VERSION, 1);
    }

    #[test]
    fn supported_list_contains_current() {
        assert!(SUPPORTED_PLUGIN_ABIS.contains(&ARK_ABI_VERSION));
    }

    #[test]
    fn abi_error_display_carries_stable_code() {
        let e = AbiError::HostTooOld {
            plugin: "foo".into(),
            plugin_abi: 99,
            host_abi: 1,
        };
        let s = format!("{e}");
        assert!(s.contains("error[abi/host-too-old]"), "got: {s}");

        let e = AbiError::PluginTooOld {
            plugin: "foo".into(),
            plugin_abi: 0,
            host_abi: 1,
        };
        assert!(format!("{e}").contains("error[abi/plugin-too-old]"));

        let e = AbiError::MissingVersion {
            plugin: "foo".into(),
        };
        assert!(format!("{e}").contains("error[abi/missing-version]"));
    }
}
