# ark-test-fixtures

Shared on-disk test fixtures for the ark workspace, plus the helper API used
to load them.

This crate exists so every test suite in the workspace references the same
fixtures through stable, absolute paths — no brittle
`../../fixtures/...` relative paths, no copy-pasted fixture data per crate.

## Layout

```
crates/test-fixtures/
├── Cargo.toml
├── README.md                     <- this file
├── src/
│   └── lib.rs                    <- `paths` + `loaders` modules
└── tests/
    └── fixtures/
        ├── cavekit-project/      <- T-111: minimal cavekit cwd (sites, impl-*.md, ralph-loop, findings)
        ├── claude-transcripts/   <- T-112: golden JSONL Claude session transcripts
        └── hook-payloads/        <- T-113: example Claude hook JSON, one per supported event
```

Each fixture subdirectory ships with a `.gitkeep` so the path exists even
before its task populates real data. The loader helpers will then start
finding real fixture files without any code changes on the consumer side.

## API

Two modules, both `pub`:

- `ark_test_fixtures::paths` — absolute path string constants
  (`FIXTURES_ROOT`, `CAVEKIT_PROJECT`, `CLAUDE_TRANSCRIPTS`, `HOOK_PAYLOADS`),
  all resolved via `env!("CARGO_MANIFEST_DIR")`.
- `ark_test_fixtures::loaders` — convenience readers:
  - `load_transcript_line(name)` — reads `claude-transcripts/{name}.jsonl`.
  - `load_hook_payload(event)` — reads `hook-payloads/{event}.json`.
  - `cavekit_fixture_dir()` — returns `PathBuf` for the cavekit-project dir.

Loaders panic on IO error with a message naming the missing fixture; tests
should fail loudly when a fixture goes missing rather than silently degrading.

## Adding a new fixture

1. Drop the file into the appropriate `tests/fixtures/<category>/` directory.
   Keep it small and human-readable wherever possible.
2. If a new category is needed, add:
   - a new subdirectory under `tests/fixtures/` with a `.gitkeep`,
   - a matching `pub const` in `src/lib.rs` `paths` module,
   - a matching loader helper in the `loaders` module, and
   - a bullet in the layout section above.
3. Document anything non-obvious about the fixture (provenance, expected
   ordering, redacted fields) inline in the file or in a sibling `NOTES.md`.
4. Commit the fixture alongside the code that depends on it.

## Consumers

The spec (`context/kits/cavekit-testing.md` R2) calls out these consumers:

- Engine contract suite (T-114) — hook payloads + transcripts.
- Orchestrator contract suite (T-115) — cavekit-project fixture directory.
- Engine unit tests (`ark-engines-claude-code`) — hook payloads.
- Orchestrator unit tests (`ark-orchestrators-cavekit`) — cavekit-project.

Crates pull this crate in as a `dev-dependency` (wiring lands in T-114+).

## Why a dedicated crate

Centralizing fixtures behind path constants means:

- Every consumer resolves paths the same way, regardless of the working
  directory `cargo test` was invoked from.
- Rename or relocate a fixture once, in one place.
- Adding a new fixture category is a small, reviewable diff that shows up in
  both the path constant list and the loader API.
