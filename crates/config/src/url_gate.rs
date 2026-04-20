//! T-PP-039 (cavekit-plugin-protocol R12): URL scheme parser + gate.
//!
//! v1 only admits two URL schemes for plugin `location=` values:
//!
//! - `file:` — local filesystem path. Kit R12 accepts FOUR spellings:
//!   - `file:///abs/path`   — canonical, triple-slash (empty authority + abs path).
//!   - `file:/abs/path`     — single-slash shorthand (no authority; RFC 3986 allows
//!     this for a local absolute path).
//!   - `file://~/rel/path`  — double-slash with `~` as the authority-shaped token,
//!     home-expanded against `$HOME`.
//!   - `file:~/rel/path`    — single-colon `~`-shorthand, same home-expansion as above.
//!
//!   The single thing rejected is `file:<bare-relative>` — `file:foo.wasm`,
//!   `file:plugins/p.wasm`, `file://./x` — anything that is neither absolute
//!   (`/…`) nor home-relative (`~/…` or bare `~`).
//!
//! - `https://` — permitted; downloaded into the content-addressed cache
//!   by `ark-host` (cache wiring is its own tier, NOT this module). The
//!   `//` is REQUIRED — a single-colon shorthand like `https:example.com`
//!   is refused up front instead of discovered at fetch time. After the
//!   `//`, the host portion must be non-empty and consist of hostname
//!   characters (`[a-zA-Z0-9.-]+`) with an optional `:PORT` suffix.
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

    /// `https://` URL with an empty, whitespace-containing, or
    /// otherwise-non-hostname host portion.
    #[error("error[ark-kdl/invalid-url]: https:// URL {raw:?} has an invalid host component")]
    HttpsInvalidHost { raw: String },
}

/// Parse a raw plugin `location=` string into a [`PluginUrl`].
///
/// Accepts `file:` (absolute or `~`-prefixed, with or without the `//`
/// authority separator) and `https://`. Refuses `http:` with a targeted
/// diagnostic; refuses every other scheme with
/// [`UrlGateError::UnsupportedScheme`].
///
/// # Examples
///
/// ```ignore
/// use ark_config::parse_plugin_url;
/// parse_plugin_url("file:///abs/path/plugin.wasm").unwrap();
/// parse_plugin_url("file:/abs/path/plugin.wasm").unwrap();
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
        "https" => parse_https_url(rest, raw),
        "http" => Err(UrlGateError::HttpNotAllowed),
        other => Err(UrlGateError::UnsupportedScheme {
            scheme: other.to_string(),
        }),
    }
}

/// Expand a `file:`-scheme URL payload.
///
/// KDL strings reach this function without the leading `file:`. Kit R12
/// accepts four equivalent spellings, all of which normalise to the
/// canonical `file:///abs/path` form:
///
/// - `file:///abs/path`    → payload `//`-stripped twice → `/abs/path`.
/// - `file:/abs/path`      → payload starts with `/` (no `//`).
/// - `file://~/rel/path`   → payload is `//~/...` → strip `//`, home-expand.
/// - `file:~/rel/path`     → payload is `~/...`, home-expand.
///
/// The ONE thing refused is a bare-relative payload: `file:foo.wasm`,
/// `file:plugins/x.wasm`, `file://./x`, `file://plugins/x`. Anything
/// that after the (optional) `//` authority-separator is neither
/// absolute (`/…`) nor home-relative (`~/…` or bare `~`).
fn parse_file_url(rest: &str, home: Option<&str>) -> Result<PluginUrl, UrlGateError> {
    // First, peel off an optional `//` authority separator. Both
    // `file://<body>` and `file:<body>` are accepted — the only
    // semantic requirement is that `<body>` is either absolute or
    // home-relative once we have it in hand.
    let payload = rest.strip_prefix("//").unwrap_or(rest);

    if payload.is_empty() {
        return Err(UrlGateError::FileNotAbsolute {
            path: payload.to_string(),
        });
    }

    // After the (optional) `//`, the payload must be one of:
    // - `~/...` / `~`   → home-expand against $HOME
    // - `/...`          → absolute path, accept as-is
    // - anything else   → bare-relative, refused
    let expanded = if let Some(tail) = payload.strip_prefix('~') {
        // `tail` is whatever follows the `~`: "/sub/path", "", or more.
        let home = home.ok_or_else(|| UrlGateError::HomeUnset {
            raw: format!("file://{payload}"),
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

/// Parse the `https:` branch. Requires the `//` authority separator and
/// then a non-empty, well-formed host portion.
///
/// The host portion is whatever comes between the `//` and the first
/// `/`, `?`, or `#` delimiter. It must:
///
/// 1. Be non-empty (rejects `https:///path` and `https://?x`).
/// 2. Contain no whitespace (rejects `https:// /x`).
/// 3. Match `^[a-zA-Z0-9.-]+(:\d+)?$` — hostname characters plus an
///    optional decimal port. We hand-roll the check rather than pulling
///    in `regex`; the grammar is small.
fn parse_https_url(rest: &str, raw: &str) -> Result<PluginUrl, UrlGateError> {
    let body = rest.strip_prefix("//").ok_or_else(|| UrlGateError::Malformed {
        raw: raw.to_string(),
    })?;

    // Split the body on the first `/`, `?`, or `#` — the prefix is the
    // host[:port] portion. An empty body (`https://`) and an empty host
    // before a delimiter (`https:///path`, `https://?x`, `https://#frag`)
    // both fail the non-empty check below.
    let host_len = body
        .find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(body.len());
    let host = &body[..host_len];

    if host.is_empty() {
        return Err(UrlGateError::HttpsInvalidHost {
            raw: raw.to_string(),
        });
    }

    // Optional `:PORT` suffix — split once and check each half separately.
    let (host_name, port_opt) = match host.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (host, None),
    };

    if host_name.is_empty() {
        return Err(UrlGateError::HttpsInvalidHost {
            raw: raw.to_string(),
        });
    }

    let host_ok = host_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !host_ok {
        return Err(UrlGateError::HttpsInvalidHost {
            raw: raw.to_string(),
        });
    }

    if let Some(port) = port_opt {
        if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
            return Err(UrlGateError::HttpsInvalidHost {
                raw: raw.to_string(),
            });
        }
    }

    Ok(PluginUrl {
        scheme: UrlScheme::Https,
        raw: raw.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- file:// — all four kit-R12 forms accepted --------------------

    #[test]
    fn file_triple_slash_absolute_parses() {
        // file:///abs/path — canonical form with empty authority.
        let u = parse_plugin_url("file:///abs/path/plugin.wasm").unwrap();
        assert_eq!(u.scheme(), UrlScheme::File);
        assert_eq!(u.as_str(), "file:///abs/path/plugin.wasm");
    }

    #[test]
    fn file_single_slash_absolute_parses() {
        // file:/abs/path — RFC 3986 single-slash shorthand. Kit R12 accepts it;
        // `crates/cli/src/commands/doctor.rs::status_plugin_kdl_snippet`
        // emits this exact form.
        let u = parse_plugin_url("file:/abs/path/plugin.wasm").unwrap();
        assert_eq!(u.scheme(), UrlScheme::File);
        assert_eq!(u.as_str(), "file:///abs/path/plugin.wasm");
    }

    #[test]
    fn file_double_slash_tilde_expands() {
        // file://~/rel — authority-style `~`, home-expanded.
        let u = parse_plugin_url_with_home("file://~/plugins/p.wasm", Some("/Users/alice"))
            .unwrap();
        assert_eq!(u.scheme(), UrlScheme::File);
        assert_eq!(u.as_str(), "file:///Users/alice/plugins/p.wasm");
    }

    #[test]
    fn file_bare_tilde_expands() {
        // file:~/rel — shorthand tilde, home-expanded (kit R12).
        let u = parse_plugin_url_with_home("file:~/plugins/p.wasm", Some("/Users/alice"))
            .unwrap();
        assert_eq!(u.scheme(), UrlScheme::File);
        assert_eq!(u.as_str(), "file:///Users/alice/plugins/p.wasm");
    }

    #[test]
    fn file_tilde_without_home_errors() {
        let err = parse_plugin_url_with_home("file://~/plugins/p.wasm", None).unwrap_err();
        assert!(matches!(err, UrlGateError::HomeUnset { .. }));
    }

    #[test]
    fn file_bare_tilde_without_home_errors() {
        // Same error path for the single-colon shorthand.
        let err = parse_plugin_url_with_home("file:~/plugins/p.wasm", None).unwrap_err();
        assert!(matches!(err, UrlGateError::HomeUnset { .. }));
    }

    // ---- file:// — refused bare-relative forms ------------------------

    #[test]
    fn file_bare_relative_refused() {
        // file:plugins/p.wasm — no `/` or `~` after the colon. Rejected.
        let err = parse_plugin_url("file:plugins/p.wasm").unwrap_err();
        assert!(
            matches!(err, UrlGateError::FileNotAbsolute { .. }),
            "expected FileNotAbsolute for bare-relative `file:plugins/p.wasm`, got {err:?}"
        );
    }

    #[test]
    fn file_bare_name_refused() {
        // file:foo.wasm — completely bare, no path separator at all.
        let err = parse_plugin_url("file:foo.wasm").unwrap_err();
        assert!(
            matches!(err, UrlGateError::FileNotAbsolute { .. }),
            "expected FileNotAbsolute for `file:foo.wasm`, got {err:?}"
        );
    }

    #[test]
    fn file_two_slashes_relative_refused() {
        // file://plugins/p.wasm — has `//` but payload is neither `/...`
        // nor `~...`. Must surface FileNotAbsolute.
        let err = parse_plugin_url("file://plugins/p.wasm").unwrap_err();
        assert!(
            matches!(err, UrlGateError::FileNotAbsolute { .. }),
            "expected FileNotAbsolute for `file://plugins/p.wasm`, got {err:?}"
        );
    }

    #[test]
    fn file_double_slash_dot_relative_refused() {
        // file://./relative — double-slash, dot-relative. Rejected.
        let err = parse_plugin_url("file://./relative").unwrap_err();
        assert!(
            matches!(err, UrlGateError::FileNotAbsolute { .. }),
            "expected FileNotAbsolute for `file://./relative`, got {err:?}"
        );
    }

    #[test]
    fn file_empty_refused() {
        // `file:` alone (empty payload) — reject.
        let err = parse_plugin_url("file:").unwrap_err();
        assert!(
            matches!(err, UrlGateError::FileNotAbsolute { .. }),
            "expected FileNotAbsolute for empty `file:`, got {err:?}"
        );
    }

    // ---- https:// — host validation ----------------------------------

    #[test]
    fn https_passes_through() {
        let u = parse_plugin_url("https://example.com/plugin.wasm").unwrap();
        assert_eq!(u.scheme(), UrlScheme::Https);
        assert_eq!(u.as_str(), "https://example.com/plugin.wasm");
    }

    #[test]
    fn https_with_port_accepted() {
        let u = parse_plugin_url("https://example.com:8443/plugin.wasm").unwrap();
        assert_eq!(u.scheme(), UrlScheme::Https);
        assert_eq!(u.as_str(), "https://example.com:8443/plugin.wasm");
    }

    #[test]
    fn https_host_only_accepted() {
        // `https://example.com` — no path. The fetcher defaults the path;
        // the gate just validates the scheme+host shape.
        let u = parse_plugin_url("https://example.com").unwrap();
        assert_eq!(u.scheme(), UrlScheme::Https);
        assert_eq!(u.as_str(), "https://example.com");
    }

    #[test]
    fn https_empty_host_refused() {
        // `https:///path` — empty host before the path delimiter.
        let err = parse_plugin_url("https:///plugin.wasm").unwrap_err();
        assert!(
            matches!(err, UrlGateError::HttpsInvalidHost { .. }),
            "expected HttpsInvalidHost for `https:///plugin.wasm`, got {err:?}"
        );
    }

    #[test]
    fn https_query_only_no_host_refused() {
        // `https://?x` — empty host before the query delimiter.
        let err = parse_plugin_url("https://?x").unwrap_err();
        assert!(
            matches!(err, UrlGateError::HttpsInvalidHost { .. }),
            "expected HttpsInvalidHost for `https://?x`, got {err:?}"
        );
    }

    #[test]
    fn https_whitespace_host_refused() {
        // `https:// /plugin.wasm` — space in the host.
        let err = parse_plugin_url("https:// /plugin.wasm").unwrap_err();
        assert!(
            matches!(err, UrlGateError::HttpsInvalidHost { .. }),
            "expected HttpsInvalidHost for whitespace host, got {err:?}"
        );
    }

    #[test]
    fn https_requires_double_slash() {
        // v1 contract: `https:not-a-url` is refused at the scheme gate.
        let err = parse_plugin_url("https:not-a-url").unwrap_err();
        assert!(
            matches!(err, UrlGateError::Malformed { .. }),
            "expected Malformed for `https:not-a-url`, got {err:?}"
        );
    }

    #[test]
    fn https_shorthand_colon_host_refused() {
        // `https:example.com/x.wasm` — single-colon shorthand.
        let err = parse_plugin_url("https:example.com/x.wasm").unwrap_err();
        assert!(
            matches!(err, UrlGateError::Malformed { .. }),
            "expected Malformed for `https:example.com/x.wasm`, got {err:?}"
        );
    }

    #[test]
    fn https_empty_body_refused() {
        // `https://` with no host at all → invalid host.
        let err = parse_plugin_url("https://").unwrap_err();
        assert!(
            matches!(err, UrlGateError::HttpsInvalidHost { .. }),
            "expected HttpsInvalidHost for `https://`, got {err:?}"
        );
    }

    #[test]
    fn https_empty_port_refused() {
        // `https://example.com:/x` — explicit empty port.
        let err = parse_plugin_url("https://example.com:/x").unwrap_err();
        assert!(
            matches!(err, UrlGateError::HttpsInvalidHost { .. }),
            "expected HttpsInvalidHost for empty port, got {err:?}"
        );
    }

    #[test]
    fn https_non_numeric_port_refused() {
        // `https://example.com:abc/x` — port must be digits only.
        let err = parse_plugin_url("https://example.com:abc/x").unwrap_err();
        assert!(
            matches!(err, UrlGateError::HttpsInvalidHost { .. }),
            "expected HttpsInvalidHost for non-numeric port, got {err:?}"
        );
    }

    // ---- scheme-level errors ------------------------------------------

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
    fn scheme_is_case_insensitive() {
        let u = parse_plugin_url("HTTPS://example.com/p.wasm").unwrap();
        assert_eq!(u.scheme(), UrlScheme::Https);
        let err = parse_plugin_url("HTTP://example.com/p.wasm").unwrap_err();
        assert!(matches!(err, UrlGateError::HttpNotAllowed));
    }

    #[test]
    fn file_scheme_case_insensitive_single_slash() {
        // `FILE:/abs/x` — uppercase scheme + single-slash shorthand still
        // parses (belt-and-braces on the R12 form acceptance).
        let u = parse_plugin_url("FILE:/abs/x").unwrap();
        assert_eq!(u.scheme(), UrlScheme::File);
        assert_eq!(u.as_str(), "file:///abs/x");
    }

    // ---- doctor.rs emits `file:/abs/path`; lock it in -----------------

    #[test]
    fn doctor_single_slash_form_accepted() {
        // Regression guard: `crates/cli/src/commands/doctor.rs::
        // status_plugin_kdl_snippet` emits `file:{abs-path}` (no `//`),
        // yielding `file:/abs/path/…`. That form MUST parse cleanly.
        let u = parse_plugin_url("file:/Users/alice/.local/share/ark/plugins/ark-status.wasm")
            .unwrap();
        assert_eq!(u.scheme(), UrlScheme::File);
        assert_eq!(
            u.as_str(),
            "file:///Users/alice/.local/share/ark/plugins/ark-status.wasm"
        );
    }
}
