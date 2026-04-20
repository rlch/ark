//! `ark-render-terminal` — terminal materializer.
//!
//! T-PP-005 (cavekit-plugin-protocol R10): signature placeholder for
//! the terminal-target materializer. Takes a widget-tree (typed per
//! T-PP-014) plus viewport size, returns bytes ready for the
//! terminal.
//!
//! The real implementation lands in T-PP-047; Tier 0 ships a stub
//! with the intended function shape so downstream callers can wire
//! against the symbol.

/// Materialize a widget-tree into terminal bytes.
///
/// Signature is placeholder — `&()` stands in for the real widget-tree
/// type (declared in `ark-plugin-protocol/wit/widget-tree.wit` +
/// projected into Rust in T-PP-014). `w` + `h` are the viewport size
/// in terminal cells.
///
/// Replaced in T-PP-047 with:
///
/// ```ignore
/// pub fn materialize(tree: &TerminalWidgetTree, w: u16, h: u16) -> Vec<u8>
/// ```
pub fn materialize(_tree: &(), _w: u16, _h: u16) -> Vec<u8> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_materialize_returns_empty_bytes() {
        assert!(materialize(&(), 80, 24).is_empty());
    }
}
