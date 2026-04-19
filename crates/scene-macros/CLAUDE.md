# crates/scene-macros

Proc-macro crate exposing `validate_scene!` — compile-time validator for
KDL scene fragments against inline manifest declarations. Emits
`.kdl:line:col` pointers via `compile_error!`.

Implements:
- cavekit-soul-phase-2-tests.md R5 (view-type compile-error goldens)
- cavekit-soul-phase-2-tests.md R6 (intent validation goldens — added 2026-04-18)

Build tasks: T-041 (phase-2 tests) + T-042 intent validator extension
Impl tracking: context/impl/impl-soul-phase-2.md
