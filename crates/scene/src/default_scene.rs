//! T-110 + T-111: Embedded default scene and user override resolution.
//!
//! [`parse_default_scene`] returns the built-in default scene that ships
//! inside the binary via `include_str!`. [`resolve_default_scene`] layers
//! a user-override path on top: if `$XDG_CONFIG_HOME/ark/scenes/default.kdl`
//! exists it wins, otherwise the built-in is used.

use std::path::Path;

use crate::error::SceneError;
use crate::parse::{parse_scene, SceneIR};

/// Raw KDL source of the built-in default scene, embedded at compile time.
pub const DEFAULT_SCENE_KDL: &str = include_str!("assets/default.kdl");

/// Parse the built-in default scene.
pub fn parse_default_scene() -> Result<SceneIR, SceneError> {
    parse_scene(DEFAULT_SCENE_KDL, "<built-in>")
}

/// Resolve default scene: user override first, then built-in.
///
/// If `xdg_config` is `Some`, checks for
/// `<xdg_config>/ark/scenes/default.kdl`. When that file exists and is
/// readable, it is parsed and returned. Otherwise falls back to
/// [`parse_default_scene`].
pub fn resolve_default_scene(xdg_config: Option<&Path>) -> Result<SceneIR, SceneError> {
    if let Some(config) = xdg_config {
        let user_path = config.join("ark/scenes/default.kdl");
        if user_path.exists() {
            let content = std::fs::read_to_string(&user_path).map_err(|e| {
                SceneError::IncludeNotFound {
                    target: user_path.display().to_string(),
                    reason: e.to_string(),
                }
            })?;
            return parse_scene(content, user_path);
        }
    }
    parse_default_scene()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::layout::LayoutChild;
    use crate::ast::SceneBodyNode;
    use tempfile::TempDir;

    /// Helper: extract the first `LayoutNode` from a scene's body.
    fn extract_layout(ir: &SceneIR) -> &crate::ast::LayoutNode {
        ir.scene
            .body
            .iter()
            .find_map(|n| match n {
                SceneBodyNode::Layout(l) => Some(l),
                _ => None,
            })
            .expect("scene should have a layout block")
    }

    #[test]
    fn default_scene_parses() {
        let ir = parse_default_scene().expect("built-in default scene should parse");
        assert_eq!(ir.scene.name, "default");
    }

    #[test]
    fn default_scene_structure() {
        let ir = parse_default_scene().expect("built-in default scene should parse");
        let layout = extract_layout(&ir);

        // Exactly one tab.
        assert_eq!(layout.tabs.len(), 1, "expected 1 tab");

        // The tab should contain a col with 2 panes.
        let tab = &layout.tabs[0];
        assert_eq!(tab.body.len(), 1, "expected 1 col inside tab");

        let col_children = match &tab.body[0] {
            LayoutChild::Col(c) => &c.body,
            other => panic!("expected Col, got {other:?}"),
        };
        assert_eq!(col_children.len(), 2, "expected 2 panes inside col");
    }

    #[test]
    fn user_override_takes_precedence() {
        let tmp = TempDir::new().unwrap();
        let scenes_dir = tmp.path().join("ark/scenes");
        std::fs::create_dir_all(&scenes_dir).unwrap();
        std::fs::write(
            scenes_dir.join("default.kdl"),
            r#"scene "custom" {
    layout {
        tab "@only" focus="true" {
            pane "@p" {
                shell
            }
        }
    }
}
"#,
        )
        .unwrap();

        let ir = resolve_default_scene(Some(tmp.path()))
            .expect("user override should parse");
        assert_eq!(ir.scene.name, "custom");
    }

    #[test]
    fn fallback_when_user_file_missing() {
        let tmp = TempDir::new().unwrap();
        // No ark/scenes/default.kdl created — should fall back to built-in.
        let ir = resolve_default_scene(Some(tmp.path()))
            .expect("should fall back to built-in");
        assert_eq!(ir.scene.name, "default");
    }
}
