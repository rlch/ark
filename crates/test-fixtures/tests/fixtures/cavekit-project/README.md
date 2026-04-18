# cavekit-project fixture

Minimal cavekit-shaped project tree (historically consumed by the
ark orchestrator crate's contract tests T-114/T-115; crate removed in
cleanup Packet A Tier 1, fixture preserved for future extension reuse).

## Layout

```
cavekit-project/
├── README.md
├── ralph-loop.md                        <- canonical ralph-loop doc (iteration + status)
├── .claude/
│   └── ralph-loop.local.md              <- watched by ralph_loop watcher
└── context/
    ├── plans/
    │   └── build-site.md                <- primary build site (8 rows)
    ├── sites/TEST-001/
    │   └── build-site.md                <- secondary/example site with mermaid DAG
    └── impl/
        ├── CLAUDE.md
        ├── impl-overview.md             <- tier progress + activity log
        ├── impl-review-findings.md      <- codex findings table (F-001, F-002, F-003)
        └── codex-findings/
            └── 2026-04-15.md            <- per-cycle findings archive
```

All files are small (~15-30 lines) and syntactically valid where structure
matters (mermaid blocks, markdown tables, YAML front-matter). Prose is
filler.
