# crates/ark-view

Typed view-model crate for ark. Defines `View`/`CommandView`/`ZellijView`
marker traits, `Pane<V>`/`Stack<V>`/`TabHandle` typed wrappers, `HandleKind`,
`HandleId`, `InvalidationCause`, `ParamsHash`, and `SessionHandles` lookup.

Implements:
- cavekit-soul-phase-2.md R2 (handle types)
- cavekit-soul-phase-2.md R4+R5 (typed wrappers + marker-gated affordances)
- cavekit-soul-phase-2.md R7 (invalidation wire shape)
- cavekit-soul-phase-2.md R8 (params hashing)
- cavekit-soul-phase-2.md R10 (name-indexed session handle lookup)

Build site: build-site-soul-phase-2.md (45/45, closed 2026-04-18)
Impl tracking: context/impl/impl-soul-phase-2.md
