# crates/ark-ext-test-support

Harness crate for Phase 2 extension conformance — subprocess stub variants,
NDJSON parity fixtures, and the shared test support surface used by
`crates/supervisor` version-mismatch matrices.

Implements:
- cavekit-soul-phase-2-tests.md R1 (stub harness)
- cavekit-soul-phase-2-tests.md R2 (subprocess stub + NDJSON parity)

Build tasks: T-037, T-038 (+ consumed by T-039 supervisor matrix)
Impl tracking: context/impl/impl-soul-phase-2.md
