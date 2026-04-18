//! Extract [`ExtensionMetadata`] from a `.wasm` file's `ark.metadata`
//! custom section (T-098).
//!
//! Extension authors embed KDL-encoded metadata into the wasm binary via
//! `#[link_section = "ark.metadata"]`. At load time this module walks the
//! wasm payload stream with [`wasmparser::Parser`], locates the custom
//! section, and parses the KDL text into an [`ExtensionMetadata`] via the
//! existing [`parse_extension_metadata_kdl`] helper.

use crate::{ExtensionMetadata, parse_extension_metadata_kdl};

/// Name of the wasm custom section that carries extension metadata.
const SECTION_NAME: &str = "ark.metadata";

/// Errors that can occur when reading the `ark.metadata` custom section.
#[derive(Debug)]
pub enum WasmMetaError {
    /// The wasm binary does not contain an `ark.metadata` custom section.
    SectionNotFound,
    /// The custom section's bytes are not valid UTF-8.
    InvalidUtf8(std::str::Utf8Error),
    /// The custom section's text is not valid KDL or does not parse into
    /// an [`ExtensionMetadata`].
    InvalidKdl(String),
    /// `wasmparser` encountered an error while walking the wasm stream.
    WasmParse(String),
}

impl std::fmt::Display for WasmMetaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SectionNotFound => {
                write!(f, "wasm binary has no `{SECTION_NAME}` custom section")
            }
            Self::InvalidUtf8(e) => {
                write!(f, "`{SECTION_NAME}` section is not valid UTF-8: {e}")
            }
            Self::InvalidKdl(e) => {
                write!(f, "`{SECTION_NAME}` section contains invalid KDL: {e}")
            }
            Self::WasmParse(e) => {
                write!(f, "failed to parse wasm binary: {e}")
            }
        }
    }
}

impl std::error::Error for WasmMetaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidUtf8(e) => Some(e),
            _ => None,
        }
    }
}

/// Extract [`ExtensionMetadata`] from the `ark.metadata` custom section
/// of a `.wasm` file.
///
/// The function uses [`wasmparser::Parser`] in streaming mode to walk the
/// binary payload. The first custom section named `ark.metadata` is
/// decoded as UTF-8 KDL text and parsed via [`parse_extension_metadata_kdl`].
///
/// # Errors
///
/// - [`WasmMetaError::SectionNotFound`] — no `ark.metadata` section exists.
/// - [`WasmMetaError::InvalidUtf8`] — section bytes are not valid UTF-8.
/// - [`WasmMetaError::InvalidKdl`] — KDL text failed to parse.
/// - [`WasmMetaError::WasmParse`] — `wasmparser` could not parse the binary.
pub fn read_wasm_metadata(wasm_bytes: &[u8]) -> Result<ExtensionMetadata, WasmMetaError> {
    let parser = wasmparser::Parser::new(0);

    for payload in parser.parse_all(wasm_bytes) {
        let payload = payload.map_err(|e| WasmMetaError::WasmParse(e.to_string()))?;

        if let wasmparser::Payload::CustomSection(reader) = payload {
            if reader.name() == SECTION_NAME {
                let kdl_text =
                    std::str::from_utf8(reader.data()).map_err(WasmMetaError::InvalidUtf8)?;
                let meta = parse_extension_metadata_kdl(kdl_text)
                    .map_err(|e| WasmMetaError::InvalidKdl(e.to_string()))?;
                return Ok(meta);
            }
        }
    }

    Err(WasmMetaError::SectionNotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a LEB128 unsigned integer into `buf`.
    fn leb128_encode(mut val: usize, buf: &mut Vec<u8>) {
        loop {
            let mut byte = (val & 0x7F) as u8;
            val >>= 7;
            if val != 0 {
                byte |= 0x80;
            }
            buf.push(byte);
            if val == 0 {
                break;
            }
        }
    }

    /// Build a minimal valid wasm module with a custom `ark.metadata`
    /// section containing the given KDL text. Hand-crafts the bytes
    /// directly — no WAT/wasm-encoder dependency needed.
    fn wasm_with_metadata(kdl: &str) -> Vec<u8> {
        let name = SECTION_NAME.as_bytes();
        let data = kdl.as_bytes();

        // Custom section payload = name_len (LEB128) + name + data
        let mut payload = Vec::new();
        leb128_encode(name.len(), &mut payload);
        payload.extend_from_slice(name);
        payload.extend_from_slice(data);

        // Wasm module = magic + version + (section_id + section_len + payload)*
        let mut wasm = Vec::new();
        // Magic number + version 1
        wasm.extend_from_slice(b"\0asm");
        wasm.extend_from_slice(&1u32.to_le_bytes());
        // Custom section id = 0
        wasm.push(0);
        leb128_encode(payload.len(), &mut wasm);
        wasm.extend_from_slice(&payload);

        wasm
    }

    /// Build a minimal wasm module with NO custom sections.
    fn wasm_without_metadata() -> Vec<u8> {
        let mut wasm = Vec::new();
        wasm.extend_from_slice(b"\0asm");
        wasm.extend_from_slice(&1u32.to_le_bytes());
        wasm
    }

    #[test]
    fn reads_metadata_from_custom_section() {
        // Use the crate's own serializer to produce valid KDL.
        use crate::{CapabilitySet, ConfigSchema, StringNode, extension_metadata_kdl_string};

        let meta = ExtensionMetadata {
            name: StringNode::new("demo"),
            version: StringNode::new("0.1.0"),
            ark_range: StringNode::new(">=0.1, <0.2"),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::default(),
            config_sections: vec![],
            reload_gates: vec![],
        };
        let kdl = extension_metadata_kdl_string(&meta).unwrap();
        let wasm = wasm_with_metadata(&kdl);
        let parsed = read_wasm_metadata(&wasm).unwrap();
        assert_eq!(parsed.name.value, "demo");
        assert_eq!(parsed.version.value, "0.1.0");
        assert_eq!(parsed.ark_range.value, ">=0.1, <0.2");
    }

    #[test]
    fn section_not_found() {
        let wasm = wasm_without_metadata();
        let err = read_wasm_metadata(&wasm).unwrap_err();
        assert!(matches!(err, WasmMetaError::SectionNotFound));
    }

    #[test]
    fn invalid_kdl_in_section() {
        // Embed garbage that is valid UTF-8 but not valid KDL for ExtensionMetadata.
        let wasm = wasm_with_metadata("not { valid { extension-kdl } }");
        let err = read_wasm_metadata(&wasm).unwrap_err();
        assert!(matches!(err, WasmMetaError::InvalidKdl(_)));
    }

    #[test]
    fn invalid_wasm_bytes() {
        let err = read_wasm_metadata(b"not a wasm file").unwrap_err();
        assert!(matches!(err, WasmMetaError::WasmParse(_)));
    }

    #[test]
    fn round_trip_through_wasm_preserves_fields() {
        // Use the crate's own serializer to produce the KDL, then embed
        // it in wasm and read it back.
        use crate::{CapabilitySet, ConfigSchema, StringNode, extension_metadata_kdl_string};

        let original = ExtensionMetadata {
            name: StringNode::new("roundtrip"),
            version: StringNode::new("1.2.3"),
            ark_range: StringNode::new(">=0.1, <0.2"),
            zellij_range: StringNode::default(),
            requires: vec![],
            intents: vec![],
            events: vec![],
            views: vec![],
            config: ConfigSchema::default(),
            capabilities: CapabilitySet::default(),
            config_sections: vec![],
            reload_gates: vec![],
        };

        let kdl_text = extension_metadata_kdl_string(&original).unwrap();
        let wasm = wasm_with_metadata(&kdl_text);
        let parsed = read_wasm_metadata(&wasm).unwrap();

        assert_eq!(parsed.name.value, original.name.value);
        assert_eq!(parsed.version.value, original.version.value);
        assert_eq!(parsed.ark_range.value, original.ark_range.value);
    }
}
