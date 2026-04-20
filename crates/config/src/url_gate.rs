//! T-PP-039 (cavekit-plugin-protocol R12): URL scheme parser + gate.
//!
//! v1 only admits two URL schemes for plugin `location=` values:
//!
//! - `file:` — absolute path OR a `~`-prefixed path expanded against
//!   `$HOME`. Loaded in place.
//! - `https:` — permitted; downloaded into the content-addressed cache
//!   by `ark-host` (cache wiring is its own tier, NOT this module).
//!
//! Everything else is an explicit refusal — `http:` especially earns a
//! bespoke diagnostic because a user typo of `http://` for `https://`
//! is the most plausible shipping-foot-gun.
//!
//! The workspace deliberately does NOT pin the `url` crate; scheme
//! gating is trivial (split on the first `:`) and the component-path
//! requirement doesn't justify pulling in another dep. If a future tier
//! needs full RFC 3986 parsing it can revisit this choice.
//!
//! # Design notes
//!
//! - `~` expansion uses `std::env::var("HOME")`. No `home` crate; on
//!   platforms where `$HOME` is unset we refuse with a dedicated error
//!   rather than silently returning the raw `~/…` string.
//! - The `PluginUrl` type stores the scheme-normalised final form
//!   (`file:///abs/path` or `https://host/path`). Callers compare on
//!   `scheme()` + `to_string()`.
//! - `oci:`, `ftp:`, and any other scheme are refused with a generic
//!   `UnsupportedScheme` error naming the offending scheme.

use std::fmt;

/// Parsed-and-accepted plugin location URL.
///
/// Stored as two pieces — the scheme tag and the post-scheme payload —
/// so callers don't have to re-parse. `raw` preserves the fully-
/// expanded string form (e.g. `file:///Users/alice/plugins/x.wasm` for
/// an input of `file://~/plugins/x.wasm`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginUrl {
    scheme: UrlScheme,
    raw: String,
}

/// The closed set of v1-accepted URL schemes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlScheme {
    /// `file:` — local filesystem path.
    File,
    /// `https:` — remote fetch (cached by `ark-host`).
    Https,
}

impl PluginUrl {
    /// Returns the parsed scheme tag.
    pub fn scheme(&self) -> UrlScheme {
        self.scheme
    }

    /// Returns the fully-expanded URL string (after `~` expansion).
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Consume the wrapper and yield the expanded string.
    pub fn into_string(self) -> String {
        self.raw
    }
}

impl fmt::Display for PluginUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

/// Errors raised by [`parse_plugin_url`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum UrlGateError {
    /// No `:` in the input, or an empty scheme.
    #[error("error[ark-kdl/invalid-url]: location {raw:?} is not a URL (expected file:// or https://)")]
    Malformed { raw: String },

    /// `http://` explicitly refused — likely a typo for `https://`.
    #[error("error[ark-kdl/invalid-url-scheme]: http:// not allowed (use https:// or file://)")]
    HttpNotAllowed,

    /// Any scheme that isn't `file:` or `https:` (and isn't the
    /// specially-handled `http:`).
    #[error(
        "error[ark-kdl/invalid-url-scheme]: scheme {scheme:?} not supported (use https:// or file://)"
    )]
    UnsupportedScheme { scheme: String },

    /// `file://~/...` input but `$HOME` isn't set.
    #[error("error[ark-kdl/invalid-url]: cannot expand ~ in {raw:?} because $HOME is unset")]
    HomeUnset { raw: String },

    /// `file:` URL with a non-absolute, non-`~` path.
    #[error("error[ark-kdl/invalid-url]: file:// path {path:?} must be absolute or start with ~")]
    FileNotAbsolute { path: String },
}

/// Parse a raw plugin `location=` string into a [`PluginUrl`].
///
/// Accepts `file:` (absolute or `~`-prefixed) and `https:`. Refuses
/// `http:` with a targeted diagnostic; refuses every other scheme with
/// [`UrlGateError::UnsupportedScheme`].
///
/// # Examples
///
/// ```ignore
/// use ark_config::parse_plugin_url;
/// parse_plugin_url("file:///abs/path/plugin.wasm").unwrap();
/// parse_plugin_url("https://example.com/p.wasm").unwrap();
/// parse_plugin_url("http://example.com/p.wasm").unwrap_err();
/// ```
pub fn parse_plugin_url(raw: &str) -> Result<PluginUrl, UrlGateError> {
    let home = std::env::var("HOME").ok();
    parse_plugin_url_with_home(raw, home.as_deref())
}

/// Test-friendly variant that takes an explicit `$HOME`. Used by the
/// unit tests so they don't race with one another by mutating the
/// process environment.
pub(crate) fn parse_plugin_url_with_home(
    raw: &str,
    home: Option<&str>,
) -> Result<PluginUrl, UrlGateError> {
    let (scheme_str, rest) = raw.split_once(':').ok_or_else(|| UrlGateError::Malformed {
        raw: raw.to_string(),
    })?;
    if scheme_str.is_empty() {
        return Err(UrlGateError::Malformed {
            raw: raw.to_string(),
        });
    }

    // Compare scheme case-insensitively per RFC 3986 §3.1.
    let scheme_lower = scheme_str.to_ascii_lowercase();
    match scheme_lower.as_str() {
        "file" => parse_file_url(rest, home),
        "https" => Ok(PluginUrl {
            scheme: UrlScheme::Https,
            raw: raw.to_string(),
        }),
        "http" => Err(UrlGateError::HttpNotAllowed),
        other => Err(UrlGateError::UnsupportedScheme {
            scheme: other.to_string(),
        }),
    }
}

/// Expand a `file:`-scheme URL payload.
///
/// KDL strings reach this function without the leading `file:`. Three
/// shapes are accepted:
///
/// - `file:///abs/path` — authority empty, absolute path
/// - `file://~/rel/path` — authority is `~`, home-expanded
/// - `file:/abs/path` — no authority, absolute path (RFC 3986 shorthand)
/// - `file:~/rel/path` — bare `~` form, home-expanded
fn parse_file_url(rest: &str, home: Option<&str>) -> Result<PluginUrl, UrlGateError> {
    // Strip the optional `//` authority separator.
    let payload = rest.strip_prefix("//").unwrap_or(rest);

    // `//~/...` → authority was the single `~` token, path follows.
    // `//~`    → authority was `~`, path empty → treat as `~` itself.
    // `~/...`  → bare tilde form after `file:` (no authority).
    // `/...`   → absolute path (no authority / empty authority).
    let expanded = if let Some(tail) = payload.strip_prefix('~') {
        // `tail` is whatever follows the `~`: "/sub/path", "", or more.
        let home = home.ok_or_else(|| UrlGateError::HomeUnset {
            raw: format!("file:{rest}"),
        })?;
        if tail.is_empty() {
            format!("file://{home}")
        } else if let Some(stripped) = tail.strip_prefix('/') {
            format!("file://{home}/{stripped}")
        } else {
            // `~foo` form is not standard and not supported here.
            return Err(UrlGateError::FileNotAbsolute {
                path: payload.to_string(),
            });
        }
    } else if payload.starts_with('/') {
        format!("file://{payload}")
    } else {
        return Err(UrlGateError::FileNotAbsolute {
            path: payload.to_string(),
        });
    };

    Ok(PluginUrl {
        scheme: UrlScheme::File,
        raw: expanded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_absolute_parses() {
        let u = parse_plugin_url("file:///abs/path/plugin.wasm").unwrap();
        assert_eq!(u.scheme(), UrlScheme::File);
        assert_eq!(u.as_str(), "file:///abs/path/plugin.wasm");
    }

    #[test]
    fn file_single_slash_absolute_parses() {
        // RFC 3986 shorthand — `file:/abs/path` with no authority.
        let u = parse_plugin_url("file:/abs/path/plugin.wasm").unwrap();
        assert_eq!(u.scheme(), UrlScheme::File);
        assert_eq!(u.as_str(), "file:///abs/path/plugin.wasm");
    }

    #[test]
    fn file_tilde_expands_against_home() {
        let u = parse_plugin_url_with_home("file://~/plugins/p.wasm", Some("/Users/alice"))
            .unwrap();
        assert_eq!(u.scheme(), UrlScheme::File);
        assert_eq!(u.as_str(), "file:///Users/alice/plugins/p.wasm");
    }

    #[test]
    fn file_bare_tilde_form_expands() {
        let u = parse_plugin_url_with_home("file:~/plugins/p.wasm", Some("/Users/alice")).unwrap();
        assert_eq!(u.as_str(), "file:///Users/alice/plugins/p.wasm");
    }

    #[test]
    fn file_tilde_without_home_errors() {
        let err = parse_plugin_url_with_home("file://~/plugins/p.wasm", None).unwrap_err();
        assert!(matches!(err, UrlGateError::HomeUnset { .. }));
    }

    #[test]
    fn https_passes_through() {
        let u = parse_plugin_url("https://example.com/plugin.wasm").unwrap();
        assert_eq!(u.scheme(), UrlScheme::Https);
        assert_eq!(u.as_str(), "https://example.com/plugin.wasm");
    }

    #[test]
    fn http_refused_with_targeted_error() {
        let err = parse_plugin_url("http://example.com/plugin.wasm").unwrap_err();
        assert!(matches!(err, UrlGateError::HttpNotAllowed));
        let msg = format!("{err}");
        assert!(
            msg.contains("http://") && msg.contains("https://") && msg.contains("file://"),
            "message should steer user to the supported schemes; got: {msg}"
        );
    }

    #[test]
    fn ftp_refused_generic() {
        let err = parse_plugin_url("ftp://example.com/plugin.wasm").unwrap_err();
        match err {
            UrlGateError::UnsupportedScheme { scheme } => assert_eq!(scheme, "ftp"),
            other => panic!("expected UnsupportedScheme, got {other:?}"),
        }
    }

    #[test]
    fn oci_refused_generic() {
        let err = parse_plugin_url("oci://registry.example.com/ark/p:1").unwrap_err();
        match err {
            UrlGateError::UnsupportedScheme { scheme } => assert_eq!(scheme, "oci"),
            other => panic!("expected UnsupportedScheme, got {other:?}"),
        }
    }

    #[test]
    fn missing_colon_malformed() {
        let err = parse_plugin_url("plugin.wasm").unwrap_err();
        assert!(matches!(err, UrlGateError::Malformed { .. }));
    }

    #[test]
    fn empty_scheme_malformed() {
        let err = parse_plugin_url(":plugin.wasm").unwrap_err();
        assert!(matches!(err, UrlGateError::Malformed { .. }));
    }

    #[test]
    fn file_relative_path_refused() {
        let err = parse_plugin_url("file:plugins/p.wasm").unwrap_err();
        match err {
            UrlGateError::FileNotAbsolute { .. } => {}
            other => panic!("expected FileNotAbsolute, got {other:?}"),
        }
    }

    #[test]
    fn scheme_is_case_insensitive() {
        let u = parse_plugin_url("HTTPS://example.com/p.wasm").unwrap();
        assert_eq!(u.scheme(), UrlScheme::Https);
        let err = parse_plugin_url("HTTP://example.com/p.wasm").unwrap_err();
        assert!(matches!(err, UrlGateError::HttpNotAllowed));
    }
}
