//! Scene compile pass (T-023 / T-024).
//!
//! [`compile_scene`] walks a parsed [`SceneIR`] and pre-compiles every
//! `when="<Rhai>"` predicate + every `{Rhai}` interpolation hole in
//! every string value that admits them. The result is a
//! [`CompiledScene`] that pairs the IR with a table of ready-to-eval
//! programs keyed by AST path (`scene.layout.tabs[0].when`, etc.).
//!
//! # Scope discipline (R8)
//!
//! - Layout subtree (tab / row / col / pane `when=`, tab `cwd` + `name`)
//!   runs in [`RhaiScope::Spawn`].
//! - Reaction / keybind subtree (`on` / `bind` `when=`, op `when=` + all
//!   op string args) runs in [`RhaiScope::Event`].
//!
//! # Static guards (T-024)
//!
//! - Maximum expression length: 4096 bytes. Redundant with the Rhai
//!   engine's `set_max_string_size(4096)` limit but caught earlier for
//!   better diagnostics.
//! - Maximum interpolation hole count per string: 32.
//!
//! # Error surfacing
//!
//! First-error fail-fast. The CLI's `ark scene check` driver is free to
//! fan out to a collect-all pass at a higher level; the library itself
//! short-circuits on the first failure.

use crate::ast::layout::{ColNode, LayoutChild, PaneNode, RowNode, TabNode};
use crate::ast::ops::OpNode;
use crate::ast::{BindNode, ModeNode, OnNode, SceneBodyNode};
use crate::error::SceneError;
use crate::interp::{InterpSegment, parse_interp};
use crate::parse::SceneIR;
use crate::rhai::{Engine, Program, RhaiScope, compile_in_scope};
use miette::{NamedSource, SourceSpan};

// Tier 4 — layout lowering to zellij KDL (T-034..T-040) and mode
// pre-rendering (T-045). Re-exported so downstream callers can write
// `use ark_scene::compile::{compile_layout_kdl, compile_modes}`.
pub mod layout;
pub mod modes;
// Tier 6 (soul phase 2) — view-type symbol table + manifest-set blake3
// hash (T-034). Scene validation queries the table to reject unknown
// `pane @h { view "..." }` references + stack/pane kind mismatches.
pub mod view_types;

pub use layout::{
    compile_layout_kdl, compile_layout_kdl_with_terminal, write_layout_artifact,
    write_layout_artifact_in,
};
pub use modes::{compile_modes, write_mode_artifacts, write_mode_artifacts_in};
pub use view_types::{
    SourceLocation, ViewEntry, ViewTypeError, ViewTypeErrorKind, ViewTypeTable, ViewTypeToken,
    manifest_set_hash, validate_view_reference,
};

/// Upper bound on the source length of any single Rhai expression (T-024).
pub const MAX_EXPR_LEN: usize = 4096;

/// Upper bound on `{Rhai}` holes per string value (T-024).
pub const MAX_INTERP_HOLES: usize = 32;

/// SceneIR + pre-compiled Rhai programs + resolved interpolation
/// segment lists. Consumed by the reconciler, reactions dispatcher,
/// and formatter without re-parsing Rhai source.
#[derive(Debug)]
pub struct CompiledScene {
    /// The underlying parsed scene.
    pub ir: SceneIR,
    /// All `when=` predicates compiled across the AST, keyed by path.
    pub predicates: Vec<(String, Program)>,
    /// All `{Rhai}` interpolation segment lists, keyed by path. Entries
    /// with zero holes are elided (literal-only strings don't need a
    /// render pass at runtime).
    pub interps: Vec<(String, Vec<InterpSegment>)>,
}

/// Compile all Rhai surfaces in `ir` and bundle the result.
#[allow(clippy::result_large_err)]
pub fn compile_scene(engine: &Engine, ir: SceneIR) -> Result<CompiledScene, SceneError> {
    let mut ctx = CompileCtx {
        engine,
        predicates: Vec::new(),
        interps: Vec::new(),
        src_path: ir.path.display().to_string(),
        src_text: ir.src.clone(),
    };
    // Walk the body in textual order, dispatching to specialized
    // walkers per node kind.
    for (i, node) in ir.scene.body.iter().enumerate() {
        let base = format!("scene.body[{i}]");
        match node {
            SceneBodyNode::Layout(layout) => {
                for (j, tab) in layout.tabs.iter().enumerate() {
                    ctx.walk_tab(tab, &format!("{base}.layout.tabs[{j}]"))?;
                }
            }
            SceneBodyNode::Mode(mode) => {
                ctx.walk_mode(mode, &format!("{base}.mode"))?;
            }
            SceneBodyNode::On(on) => {
                ctx.walk_on(on, &format!("{base}.on"))?;
            }
            SceneBodyNode::Bind(bind) => {
                ctx.walk_bind(bind, &format!("{base}.bind"))?;
            }
            // `use` / `include` / `clear-*` / `disable-extension` carry
            // no Rhai surfaces — skip.
            SceneBodyNode::Use(_)
            | SceneBodyNode::Include(_)
            | SceneBodyNode::ClearReactions(_)
            | SceneBodyNode::ClearBind(_)
            | SceneBodyNode::DisableExtension(_) => {}
        }
    }
    Ok(CompiledScene {
        ir,
        predicates: ctx.predicates,
        interps: ctx.interps,
    })
}

/// Carrier for the walker's accumulated output + static source context.
struct CompileCtx<'a> {
    engine: &'a Engine,
    predicates: Vec<(String, Program)>,
    interps: Vec<(String, Vec<InterpSegment>)>,
    src_path: String,
    src_text: String,
}

impl<'a> CompileCtx<'a> {
    fn compile_when(
        &mut self,
        when: &Option<String>,
        scope: RhaiScope,
        path: &str,
    ) -> Result<(), SceneError> {
        let Some(src) = when else { return Ok(()) };
        // Static guard: max expression length.
        if src.len() > MAX_EXPR_LEN {
            return Err(SceneError::RhaiParse {
                message: format!(
                    "expression too long: {} bytes (limit {MAX_EXPR_LEN})",
                    src.len()
                ),
                src: NamedSource::new(self.src_path.clone(), self.src_text.clone()),
                span: SourceSpan::new(0.into(), self.src_text.len().min(1)),
            });
        }
        let program = compile_in_scope(self.engine, src, scope)?;
        self.predicates.push((path.to_string(), program));
        Ok(())
    }

    fn compile_interp(
        &mut self,
        raw: &Option<String>,
        scope: RhaiScope,
        path: &str,
    ) -> Result<(), SceneError> {
        let Some(s) = raw else { return Ok(()) };
        self.compile_interp_str(s, scope, path)
    }

    fn compile_interp_str(
        &mut self,
        raw: &str,
        scope: RhaiScope,
        path: &str,
    ) -> Result<(), SceneError> {
        // Static guard: max string length (same 4096-byte cap applies).
        if raw.len() > MAX_EXPR_LEN {
            return Err(SceneError::RhaiParse {
                message: format!(
                    "interpolated string too long: {} bytes (limit {MAX_EXPR_LEN})",
                    raw.len()
                ),
                src: NamedSource::new(self.src_path.clone(), self.src_text.clone()),
                span: SourceSpan::new(0.into(), self.src_text.len().min(1)),
            });
        }
        let segments = parse_interp(self.engine, raw, scope)?;
        // Static guard: max hole count.
        let holes = segments
            .iter()
            .filter(|s| matches!(s, InterpSegment::Hole(_)))
            .count();
        if holes > MAX_INTERP_HOLES {
            return Err(SceneError::RhaiParse {
                message: format!(
                    "too many `{{Rhai}}` holes: {holes} (limit {MAX_INTERP_HOLES})"
                ),
                src: NamedSource::new(self.src_path.clone(), self.src_text.clone()),
                span: SourceSpan::new(0.into(), raw.len().max(1)),
            });
        }
        if holes > 0 {
            self.interps.push((path.to_string(), segments));
        }
        Ok(())
    }

    fn walk_tab(&mut self, tab: &TabNode, path: &str) -> Result<(), SceneError> {
        self.compile_when(&tab.when, RhaiScope::Spawn, &format!("{path}.when"))?;
        self.compile_interp(&tab.cwd, RhaiScope::Spawn, &format!("{path}.cwd"))?;
        self.compile_interp(&tab.name, RhaiScope::Spawn, &format!("{path}.name"))?;
        for (i, child) in tab.body.iter().enumerate() {
            self.walk_layout_child(child, &format!("{path}.body[{i}]"))?;
        }
        Ok(())
    }

    fn walk_mode(&mut self, mode: &ModeNode, path: &str) -> Result<(), SceneError> {
        for (i, tab) in mode.tabs.iter().enumerate() {
            self.walk_tab(tab, &format!("{path}.tabs[{i}]"))?;
        }
        Ok(())
    }

    fn walk_layout_child(
        &mut self,
        child: &LayoutChild,
        path: &str,
    ) -> Result<(), SceneError> {
        match child {
            LayoutChild::Row(row) => self.walk_row(row, path),
            LayoutChild::Col(col) => self.walk_col(col, path),
            LayoutChild::Pane(pane) => self.walk_pane(pane, path),
        }
    }

    fn walk_row(&mut self, row: &RowNode, path: &str) -> Result<(), SceneError> {
        self.compile_when(&row.when, RhaiScope::Spawn, &format!("{path}.when"))?;
        for (i, child) in row.body.iter().enumerate() {
            self.walk_layout_child(child, &format!("{path}.body[{i}]"))?;
        }
        Ok(())
    }

    fn walk_col(&mut self, col: &ColNode, path: &str) -> Result<(), SceneError> {
        self.compile_when(&col.when, RhaiScope::Spawn, &format!("{path}.when"))?;
        for (i, child) in col.body.iter().enumerate() {
            self.walk_layout_child(child, &format!("{path}.body[{i}]"))?;
        }
        Ok(())
    }

    fn walk_pane(&mut self, pane: &PaneNode, path: &str) -> Result<(), SceneError> {
        self.compile_when(&pane.when, RhaiScope::Spawn, &format!("{path}.when"))?;
        // Visit view config block for `{Rhai}` interpolation holes (F-0013).
        // View config string values (e.g. `cmd "{project.root}/bin/run"`)
        // are interpolated in the spawn scope.
        if let Some(cfg) = &pane.view.config_block {
            for node in cfg.nodes().iter() {
                for (j, entry) in node.entries().iter().enumerate() {
                    if let ::kdl::KdlValue::String(s) = entry.value() {
                        if s.contains('{') {
                            let entry_path = format!(
                                "{path}.view.{}.entries[{j}]",
                                node.name().value(),
                            );
                            self.compile_interp_str(s, RhaiScope::Spawn, &entry_path)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn walk_on(&mut self, on: &OnNode, path: &str) -> Result<(), SceneError> {
        self.compile_when(&on.when, RhaiScope::Event, &format!("{path}.when"))?;
        for (i, op) in on.ops.iter().enumerate() {
            self.walk_op(op, &format!("{path}.ops[{i}]"))?;
        }
        Ok(())
    }

    fn walk_bind(&mut self, bind: &BindNode, path: &str) -> Result<(), SceneError> {
        for (i, op) in bind.ops.iter().enumerate() {
            self.walk_op(op, &format!("{path}.ops[{i}]"))?;
        }
        Ok(())
    }

    fn walk_op(&mut self, op: &OpNode, path: &str) -> Result<(), SceneError> {
        // Every op carries `when=`; match on the variant to pick it
        // up plus any string args that admit `{Rhai}` holes (R7 + T-054).
        match op {
            OpNode::Focus(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
            }
            OpNode::Close(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
            }
            OpNode::Rename(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
                self.compile_interp_str(&o.to, RhaiScope::Event, &format!("{path}.to"))?;
            }
            OpNode::Resize(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
            }
            OpNode::Move(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
                self.compile_interp_str(&o.to, RhaiScope::Event, &format!("{path}.to"))?;
            }
            OpNode::Pin(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
            }
            OpNode::Unpin(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
            }
            OpNode::Spawn(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
            }
            OpNode::NewTab(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
                self.compile_interp(&o.name, RhaiScope::Event, &format!("{path}.name"))?;
                self.compile_interp(&o.cwd, RhaiScope::Event, &format!("{path}.cwd"))?;
            }
            OpNode::UseMode(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
            }
            OpNode::Pipe(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
                self.compile_interp_str(
                    &o.payload,
                    RhaiScope::Event,
                    &format!("{path}.payload"),
                )?;
            }
            OpNode::Emit(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
            }
            OpNode::SetStatus(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
                self.compile_interp_str(&o.text, RhaiScope::Event, &format!("{path}.text"))?;
            }
            OpNode::Exec(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
                self.compile_interp_str(
                    &o.script,
                    RhaiScope::Event,
                    &format!("{path}.script"),
                )?;
            }
            OpNode::ReloadScene(o) => {
                self.compile_when(&o.when, RhaiScope::Event, &format!("{path}.when"))?;
            }
            OpNode::Unknown { .. } => {
                // Unknown-op diagnostic is T-053's responsibility.
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_scene;

    fn compile(src: &str) -> Result<CompiledScene, SceneError> {
        let ir = parse_scene(src, "test.kdl").expect("parse should succeed");
        let engine = Engine::new();
        compile_scene(&engine, ir)
    }

    #[test]
    fn empty_scene_compiles() {
        let cs = compile(r#"scene "x" { }"#).expect("empty scene should compile");
        assert!(cs.predicates.is_empty());
        assert!(cs.interps.is_empty());
    }

    #[test]
    fn tab_when_collected() {
        let src = r#"scene "s" { layout { tab "@main" when="true" { } } }"#;
        let cs = compile(src).expect("tab when should compile");
        assert_eq!(cs.predicates.len(), 1);
        assert!(cs.predicates[0].0.contains("tab"));
        assert!(cs.predicates[0].0.ends_with("when"));
    }

    #[test]
    fn tab_cwd_interp_collected() {
        let src = r#"scene "s" { layout { tab "@main" cwd="{id}/src" { } } }"#;
        let cs = compile(src).expect("tab cwd interp should compile");
        assert_eq!(cs.interps.len(), 1);
        assert!(cs.interps[0].0.ends_with("cwd"));
        // Literal-only `name` would be elided; here cwd has one hole + one literal.
        let segs = &cs.interps[0].1;
        assert!(segs.iter().any(|s| matches!(s, InterpSegment::Hole(_))));
    }

    #[test]
    fn literal_cwd_not_collected() {
        let src = r#"scene "s" { layout { tab "@main" cwd="/literal/path" { } } }"#;
        let cs = compile(src).expect("literal cwd should compile");
        assert!(cs.interps.is_empty(), "literal strings should be elided");
    }

    #[test]
    fn on_when_collected_in_event_scope() {
        let src = r#"scene "s" { on "FileEdited" when="true" { close "@x" } }"#;
        let cs = compile(src).expect("on when should compile");
        // on.when + op.when default None for close: so 1 predicate only.
        assert_eq!(cs.predicates.len(), 1);
        assert_eq!(cs.predicates[0].1.scope, RhaiScope::Event);
    }

    #[test]
    fn invalid_when_rejects() {
        let src = r#"scene "s" { layout { tab "@main" when="1 +" { } } }"#;
        let err = compile(src).expect_err("broken Rhai must reject at compile");
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    #[test]
    fn invalid_interp_rejects() {
        let src = r#"scene "s" { layout { tab "@main" cwd="{}/src" { } } }"#;
        let err = compile(src).expect_err("empty hole must reject");
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    #[test]
    fn expression_length_guard() {
        // Build a 5000-char Rhai expression: `1 + 1 + 1 + …`.
        let mut expr = String::from("1");
        while expr.len() < 5000 {
            expr.push_str(" + 1");
        }
        let src = format!(r#"scene "s" {{ layout {{ tab "@main" when="{expr}" {{ }} }} }}"#);
        let err = compile(&src).expect_err("oversize when= must reject");
        assert!(matches!(err, SceneError::RhaiParse { .. }));
    }

    #[test]
    fn nested_layout_collects_predicates() {
        let src = r#"
scene "s" {
    layout {
        tab "@main" when="true" {
            row when="true" {
                pane "@p" when="false"
            }
        }
    }
}
"#;
        let cs = compile(src).expect("nested layout should compile");
        assert_eq!(cs.predicates.len(), 3);
        for (_, p) in &cs.predicates {
            assert_eq!(p.scope, RhaiScope::Spawn);
        }
    }

    #[test]
    fn op_interp_collected() {
        // `set_status text="hello {payload.name}"` collects one interp entry.
        let src = r#"scene "s" { on "FileEdited" { set_status text="hi {payload.name}" } }"#;
        let cs = compile(src).expect("op interp should compile");
        // Zero predicates (no `when=`) + one interp for text.
        assert!(cs.interps.iter().any(|(k, _)| k.ends_with(".text")));
    }

    #[test]
    fn bind_body_event_scope() {
        let src = r#"scene "s" { bind "Alt q" { set_status text="x {payload.name}" } }"#;
        let cs = compile(src).expect("bind body should compile");
        assert!(!cs.interps.is_empty());
    }

    // F-0013: view config Rhai holes are compiled
    #[test]
    fn pane_view_config_rhai_holes_compiled() {
        use crate::ast::layout::{LayoutChild, PaneNode, TabNode, ViewRef};
        use crate::ast::{LayoutNode, SceneBodyNode, SceneNode};
        use std::path::PathBuf;

        let cfg_src = r#"cmd "{project.root}/bin/serve""#;
        let cfg = ::kdl::KdlDocument::parse_v2(cfg_src).unwrap();

        let ir = SceneIR {
            scene: SceneNode {
                name: "s".to_string(),
                max_cascade_depth: None,
                body: vec![SceneBodyNode::Layout(LayoutNode {
                    tabs: vec![TabNode {
                        handle: "@main".to_string(),
                        cwd: None,
                        name: None,
                        focus: None,
                        when: None,
                        body: vec![LayoutChild::Pane(PaneNode {
                            handle: "@p".to_string(),
                            span: None,
                            cells: None,
                            min: None,
                            max: None,
                            when: None,
                            overlay: None,
                            view: ViewRef {
                                alias: "command".to_string(),
                                config_block: Some(cfg),
                            },
                        })],
                    }],
                })],
            },
            path: PathBuf::from("test.kdl"),
            src: String::new(),
            id: crate::id::SceneId::new("test.kdl", b"x"),
            kdl_doc: None,
        };

        let engine = Engine::new();
        let cs = compile_scene(&engine, ir).expect("view config holes should compile");
        assert!(
            cs.interps.iter().any(|(k, _)| k.contains("view.cmd")),
            "expected interp for view config cmd hole; got keys: {:?}",
            cs.interps.iter().map(|(k, _)| k).collect::<Vec<_>>()
        );
    }

    // F-0013: literal view config values with no holes are not collected
    #[test]
    fn pane_view_config_literal_not_collected() {
        use crate::ast::layout::{LayoutChild, PaneNode, TabNode, ViewRef};
        use crate::ast::{LayoutNode, SceneBodyNode, SceneNode};
        use std::path::PathBuf;

        let cfg_src = r#"cmd "/usr/bin/htop""#;
        let cfg = ::kdl::KdlDocument::parse_v2(cfg_src).unwrap();

        let ir = SceneIR {
            scene: SceneNode {
                name: "s".to_string(),
                max_cascade_depth: None,
                body: vec![SceneBodyNode::Layout(LayoutNode {
                    tabs: vec![TabNode {
                        handle: "@main".to_string(),
                        cwd: None,
                        name: None,
                        focus: None,
                        when: None,
                        body: vec![LayoutChild::Pane(PaneNode {
                            handle: "@p".to_string(),
                            span: None,
                            cells: None,
                            min: None,
                            max: None,
                            when: None,
                            overlay: None,
                            view: ViewRef {
                                alias: "command".to_string(),
                                config_block: Some(cfg),
                            },
                        })],
                    }],
                })],
            },
            path: PathBuf::from("test.kdl"),
            src: String::new(),
            id: crate::id::SceneId::new("test.kdl", b"x"),
            kdl_doc: None,
        };

        let engine = Engine::new();
        let cs = compile_scene(&engine, ir).expect("literal view config should compile");
        assert!(
            cs.interps.is_empty(),
            "literal-only config values should not produce interps"
        );
    }
}
