//! PermissionRequest auto-allow payload (T-050, cavekit-hook-ipc.md R1
//! + cavekit-engine-claude-code.md R3 permission clause).
//!
//! For now ark-hook unconditionally writes the Claude-Code allow payload
//! to stdout whenever the `PermissionRequest` hook fires. This keeps the
//! skeleton fail-open: until T-054 wires
//! `config.engine.claude_code.permission_policy`
//! (`ask` | `auto_approve_read` | `auto_approve_all`), the safest
//! behavior is "always allow, but emit a `PermissionAsked` trace event
//! first so the observability surface still records what was approved".
//!
//! ## Wire shape
//! The Claude Code docs require *exactly* this JSON on stdout:
//! ```json
//! {"hookSpecificOutput":{"decision":{"behavior":"allow"}}}
//! ```
//! Byte-equality with [`ALLOW_PAYLOAD_JSON`] is pinned by unit test.
//!
//! ## T-054 handoff
//! T-054 will consult `config.engine.claude_code.permission_policy`:
//! - `ask` → skip the allow payload; Claude prompts the user in its TUI.
//! - `auto_approve_read` → emit allow only if tool is in the read-only
//!   set (`Read`, `Glob`, `Grep`, `WebFetch`, `WebSearch`); otherwise
//!   behave as `ask`.
//! - `auto_approve_all` → always emit allow (same as today's default).
//! In every branch the `PermissionAsked` + `PermissionResolved` events
//! are still emitted via the JSONL writer and the zellij pipe so the
//! observability contract stays stable.

use std::io::{self, Write};

/// Exact JSON body written to stdout when approving a PermissionRequest.
///
/// Pinned as a `const &str` (no `to_string` allocation) and byte-tested
/// to prevent accidental drift — Claude Code parses this verbatim.
pub const ALLOW_PAYLOAD_JSON: &str = r#"{"hookSpecificOutput":{"decision":{"behavior":"allow"}}}"#;

/// Write the allow payload to the supplied writer. No trailing newline
/// — Claude's hook JSON parser expects a single document, and the
/// fixture tests in the engine crate assert byte-equality.
pub fn write_allow_payload<W: Write>(mut out: W) -> io::Result<()> {
    out.write_all(ALLOW_PAYLOAD_JSON.as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_bytes_exactly_match_doc() {
        // If this ever drifts, Claude's hook-output parser will reject
        // the decision. Byte-equal check is cheap insurance.
        assert_eq!(
            ALLOW_PAYLOAD_JSON,
            r#"{"hookSpecificOutput":{"decision":{"behavior":"allow"}}}"#
        );
    }

    #[test]
    fn writes_payload_to_buffer() {
        let mut buf: Vec<u8> = Vec::new();
        write_allow_payload(&mut buf).unwrap();
        assert_eq!(std::str::from_utf8(&buf).unwrap(), ALLOW_PAYLOAD_JSON);
    }

    #[test]
    fn payload_parses_as_expected_shape() {
        let v: serde_json::Value = serde_json::from_str(ALLOW_PAYLOAD_JSON).unwrap();
        assert_eq!(
            v.pointer("/hookSpecificOutput/decision/behavior")
                .and_then(|v| v.as_str()),
            Some("allow")
        );
    }
}
