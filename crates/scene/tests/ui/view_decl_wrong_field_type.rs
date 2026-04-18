//! T-041 compile-fail: `ViewDecl.name` is a `String`. Passing a
//! non-string literal (here, an integer) must be rejected by rustc with
//! E0308 "mismatched types". Guards against a silent field-type relax
//! on `ViewDecl` (e.g. re-typing `name` as `impl Into<String>` for
//! positional args, or adding a `From<i32>` blanket impl).

use ark_ext_metadata_types::{StringNode, ViewDecl};

fn main() {
    let _ = ViewDecl {
        name: 42,
        component: StringNode::new("X"),
        kind: None,
    };
}
