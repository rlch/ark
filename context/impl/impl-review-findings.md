# Peer Review Findings

## Latest Review: Tier 0 — 2026-04-16

**Base ref:** `7133cd2` (docs: propagate Rhai migration)
**Head:** `752e003` (T-010 scene: insta snapshot harness for SceneError diagnostics)
**Reviewer:** Codex (codex-cli 0.120.0, default ChatGPT model)
**Diff:** 3504 lines across 8 commits.

### Findings

| # | Severity | File | Line | Issue | Status |
|---|----------|------|------|-------|--------|
| F-0001 | P1 | crates/scene/src/ast/ops.rs | 16 | Op AST fields lack `#[facet(kdl::argument)]` / `#[facet(kdl::property)]`; `OpNode` variants lack renames for canonical verbs (`new_tab`, `use_mode`, `set_status`, `reload_scene`, `acp.*`). facet-kdl cannot deserialize real scene ops as written. | DEFERRED |
| F-0002 | P2 | crates/scene/src/ast/layout.rs | 56 | `Handle::new` only rejects whitespace + embedded `@`; accepts non-identifier handles like `@foo/bar`, `@-x`, `@.`. Weakens reconciler identity invariant. | FIXED |
| F-0003 | P2 | crates/scene/src/ast/selector.rs | 122 | `FieldPattern::parse` treats any `(`-prefixed value as annotation candidate. Valid exact literals starting with `(` (e.g. `tool="(foo"`) fail as malformed. | FIXED |

### Disposition

- **F-0001 deferred to T-011** — Tier 0 defined AST field shapes only. KDL binding attributes (`#[facet(kdl::argument)]` etc.) + canonical verb renames are T-011's responsibility (`parse_scene` entry point). T-005 left `OpNode` + structs as pure Rust data types; facet-kdl wiring ships with the parser that consumes them. Will be re-reviewed at Tier 1 gate.
- **F-0002 + F-0003 fixed** — see follow-up commit.
