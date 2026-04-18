//! T-041 compile-fail: `ExtensionMetadata` is a struct with required
//! fields. A struct-literal that omits `views` (and the other required
//! fields below it) must be rejected by rustc with E0063 "missing
//! fields …". Guards against ext metadata growing new required fields
//! without breaking every downstream struct-literal builder — the
//! compile-fail fixture forces those call sites to update in lockstep.

use ark_ext_metadata_types::{ExtensionMetadata, StringNode};

fn main() {
    let _ = ExtensionMetadata {
        name: StringNode::new("x"),
        version: StringNode::new("0.1.0"),
        ark_range: StringNode::new(">=0.1"),
        zellij_range: StringNode::new(""),
        requires: vec![],
        intents: vec![],
        events: vec![],
        // Missing: config, views, capabilities, config_sections, reload_gates.
    };
}
