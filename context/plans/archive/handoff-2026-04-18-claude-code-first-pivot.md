---
created: "2026-04-18"
audience: next session / receiving agent
status: handoff
supersedes_in_this_session: project_scope_cut_2026_04_18.md (partial — see "Reversals" below)
---

# Handoff — v0.1 Pivot: Claude Code First (Hook-Based), pi Second

## TL;DR

1. **Claude-code stays in v0.1.** It was fully deleted in the session earlier
   today; that deletion reverses. Claude-code moves to `extensions/claude-code/`
   (per the original soul kit Phase 4 plan), integrated through **hooks +
   transcript watching**, not ACP.
2. **Claude-code ships first, pi ships second.** Previous plan had pi as the
   sole v0.1 engine. Now claude-code is the first ext-surface consumer; pi
   becomes v0.2 work.
3. **Subagent observability DOES work** for Claude Code (investigation
   summary below). The stack + tile + log-pane pattern designed for pi
   ports cleanly to claude-code. The `claude-code` extension will ship its
   own `claude-code-subagent-tile` + `claude-code-subagent-log` views.
4. **Still deleted / still gone** from ark: ACP client, cavekit
   orchestrator, acp-client crate. Only claude-code resurrects.
5. **Scene 2026-04-18 revision stays** (typed handles `Pane<V>`/`Stack<V>`,
   `stack` primitive un-deferred, `HandleKind` narrows to
   `{Tab, Pane, Stack}`). Claude-code extension consumes it.
6. **Soul kit Phase 2 is under-specified** — gap analysis at bottom of this
   doc. Needs decomposition (~6 sub-kits, ~30 R's) to mirror Phase 1's shape.

The receiving agent should: (1) revert specific kit edits per the "Reversals"
list below; (2) write `cavekit-claude-code-ext.md` as the v0.1 engine kit;
(3) decompose soul Phase 2; (4) keep `cavekit-pi.md` on ice for v0.2.

---

## Context of the pivot

Earlier in the session on 2026-04-18 a scope cut was landed:

- **Deleted** `crates/orchestrators/claude-code/`, `crates/hook/`,
  `crates/types/src/permission.rs`, kit files for claude-code engine +
  claude-code orchestrator + cavekit orchestrator + hook-ipc.
- **Intent:** v0.1 ships bare ark + scene + pi extension family only.

User then reversed the claude-code portion of that cut: claude-code is the
right *first* engine integration, not pi. Reasons:

- Claude Code is the user's daily-driven agent; shipping it first validates
  the ext surface against the real target.
- Pi's TS bridge is novel and unproven; claude-code's hook system is well-
  understood (ark already had `crates/hook/` + `cavekit-hook-ipc.md` as
  reference designs before they were deleted this morning).
- Claude Code has a large user base; an ark+claude-code integration is
  immediately useful even if pi never lands.

Pi-first was a defensible plan; claude-code-first is a *better* plan given
user's actual workflow. The kits and investigation work from the pi session
are not wasted — they become a template for the claude-code extension.

---

## Subagent investigation — Claude Code observability

Investigated 2026-04-18 via web-search of Claude Code hook reference docs.
Primary source: https://code.claude.com/docs/en/hooks

### Key findings

1. **Subagent lifecycle is fully observable via hooks.** Events fire at:
   - `SubagentStart` — when a Task-tool subagent is spawned.
   - `SubagentStop` — when the subagent finishes.
   - `PreToolUse` / `PostToolUse` — fire inside subagents too; each call
     carries `agent_id` + `agent_type` in the hook payload.

2. **Per-subagent transcripts exist on disk.** `SubagentStop` payload
   includes `agent_transcript_path` pointing at
   `~/.claude/projects/.../<session-id>/subagents/agent-<id>.jsonl`.
   Structurally equivalent to pi-subagents' `events.jsonl` per-subagent
   file. Tail-able for live view.

3. **Final output is handed directly.** `SubagentStop` carries
   `last_assistant_message` — no need to parse transcript for the headline
   result.

4. **Hooks only fire at discrete lifecycle points.** Not a token-streaming
   model. But `PostToolUse` per-tool-call-inside-subagent plus transcript
   tailing gives near-real-time progress UX equivalent to pi's.

### Implication for ark

The `claude-code` extension can ship:
- `claude-code-subagent-tile` CommandView — stacks inside
  `Stack<ClaudeCodeSubagentTile>`; collapsed row shows
  `<agent_type> · <status> · last_tool_used`; expanded view tails the
  subagent's `agent_transcript_path`.
- `claude-code-subagent-log` CommandView — full-pane tail of a selected
  subagent's transcript.

Same view-coordination pattern as the pi kit's R6 + R11 (typed
`Stack<V>`/`Pane<V>` attrs on the parent claude-code view). The pi kit
shape is a template; rename the views, swap the event sources, wire to
hook-driven ExtEvents.

### Where Claude Code subagents DIVERGE from pi subagents

- **No runtime tool injection.** Pi has `pi.registerTool(...)` — ark can
  register new LLM-callable tools mid-session. Claude Code has no
  equivalent. The pi-kit's `ark_dispatch(kdl)` + `ark_ops()` pi-tool
  pattern doesn't port 1:1.
- **Workaround: MCP server.** Claude Code speaks MCP natively. Ark can ship
  an `ark-mcp` binary that exposes `IntentRegistry` as MCP tools; user
  configures their `.mcp.json` to include it. Same functional outcome
  (claude-code calls `ark_dispatch(kdl)` tool, routes through
  `IntentRegistry`), different integration mechanism (MCP handshake, not
  in-proc TS ext injection). This becomes a fourth `claude-code-control`
  sub-crate when we need it — v0.1 can ship without.

---

## v0.1 shape (revised)

### Must land in this order

1. **Soul Phase 1** (in flight, other agent) — types migration, bare
   launch.
2. **Soul Phase 2** (under-specified — see gap analysis) — ext-hook
   surface.
3. **Soul Phase 3** (ACP deletion) — unchanged.
4. **Soul Phase 4** (revised) — extract claude-code to ext; delete cavekit
   orchestrator.
5. **Soul Phase 5** (delete Engine/Orchestrator traits) — unchanged.
6. **Scene 2026-04-18 revision** (typed handles + stack) — unchanged.
7. **claude-code extension** — v0.1 engine integration.

### Deferred to v0.2+

- **pi extension family** (`cavekit-pi.md` stays; status → DEFERRED).
- Any ark-mcp / claude-code-control surface.
- Multi-engine coexistence (one session launches claude + pi views
  simultaneously).

---

## Reversals needed in current kit state

These were edited earlier in the session under the "pi first, no
claude-code" assumption. Each needs surgical revert.

### Must revert

1. **`context/kits/cavekit-overview.md`:**
   - Current: pi is v0.1's sole engine; claude-code + cavekit + ACP listed
     as deleted.
   - Revert to: claude-code is v0.1's first engine (as ext); pi deferred;
     cavekit + ACP still deleted.
   - Specifically:
     - Re-introduce the `Claude Code extension` row in the domain table
       (points at new `cavekit-claude-code-ext.md`, see below).
     - Update the v0.1 milestones section to include claude-code extension
       as a first-class deliverable; pi moves to v0.2.

2. **`context/kits/cavekit-soul.md` Phase 4:**
   - Current: "Delete Claude Code + Cavekit outright (2026-04-18 scope cut)"
     — deletes everything.
   - Revert to: delete `crates/orchestrators/{claude-code,cavekit}/` +
     `crates/hook/` + `crates/types/src/permission.rs` **as ark crates**,
     BUT migrate their functional content into `extensions/claude-code/`
     (restoring the original Phase 4 plan for claude-code only).
   - Cavekit orchestrator stays deleted (no extension rehoming); only
     claude-code moves to ext.

3. **`context/kits/cavekit-scene.md` 2026-04-18 changelog:**
   - Current text: "claude-code + cavekit + ACP removed as part of soul
     Phases 3 + 4."
   - Revert to: "cavekit orchestrator + ACP removed; claude-code extracted
     to `extensions/claude-code/` (hook-based integration, not ACP)."

4. **`context/kits/cavekit-pi.md`:**
   - Current motivation: "pi is the sole engine integration in v0.1."
   - Revert to: "pi is planned for v0.2; claude-code ships first as the
     reference ext-surface consumer. Pi becomes the second integration;
     the ext surface + scene revisions validated by claude-code carry pi
     through with minimal churn."
   - Add a status: DEFERRED row at top; all R1–R22 content preserved for
     v0.2.

5. **`.claude/projects/.../memory/project_scope_cut_2026_04_18.md`:**
   - Current text: full deletion of claude-code.
   - Revert to: cavekit + ACP fully deleted; claude-code moves to
     `extensions/claude-code/` with hook-based integration. Pi deferred.
   - MEMORY.md index pointer text updated to match.

6. **`context/kits/cavekit-plugin-picker.md`:**
   - Current text (from today's session): "ACP permission-modal feature
     removed (ACP deleted 2026-04-18)."
   - No further change needed — picker is engine-agnostic. Leave as-is.

### Must resurrect (as drafts to be rewritten)

These were deleted earlier today. They need to return as new specs (not
verbatim — the underlying design evolved):

1. **`context/kits/cavekit-claude-code-ext.md`** (NEW file, name TBD — was
   `cavekit-engine-claude-code.md` and `cavekit-orchestrator-claude-code.md`
   previously). Consolidates into a single extension spec. Expected shape:
   - Ships inside `extensions/claude-code/` crate.
   - Hook-based integration: `cc-hook` sub-binary (what was `ark-hook`)
     receives Claude Code hook POSTs and forwards to ark over per-session
     control socket.
   - Transcript watcher tails `~/.claude/projects/<path>/<session-id>.jsonl`
     for main session + `./subagents/agent-<id>.jsonl` for each subagent.
   - ExtEvents emitted as `claude-code.*` (e.g. `claude-code.pre-tool-use`,
     `claude-code.subagent.start`, `claude-code.subagent.stop`,
     `claude-code.permission.request`, `claude-code.stop`).
   - Views:
     - `claude-code` CommandView — launches `claude` in a pane. Typed
       attrs: `model`, `args`, `subagents: Stack<ClaudeCodeSubagentTile>`,
       `logs: Pane<ClaudeCodeSubagentLog>`, `permission: Pane<ClaudeCodePermissionView>`.
     - `claude-code-subagent-tile` CommandView — stack child; collapsed
       shows `<agent_type> · <status>`; expanded tails
       `agent_transcript_path`.
     - `claude-code-subagent-log` CommandView — full pane tail of a
       selected subagent transcript.
     - `claude-code-permission` view — renders pending permission requests
       (from `PermissionRequest` hook); y/n keybinds resolve via reverse
       channel back to `cc-hook`'s IPC socket.
   - Scene-compile hook: injects `CLAUDE_HOOK_SOCKET=<path>` into panes
     launching `claude` so the user's installed hook binary can forward
     POSTs.
   - Doctor checks: `claude` on PATH, hook binary installed + up-to-date,
     socket writable, user's `~/.claude/settings.json` hooks configured.
   - Config:
     ```toml
     [claude-code]
     tool_policy = "prompt"     # "prompt" | "auto-approve-safe" | "deny-all"
     read_only_tools = ["Read", "Glob", "Grep", "WebFetch", "WebSearch"]
     transcript_tail_lines = 200
     ```
   - Restores what was in `cavekit-engine-claude-code.md` but shaped as an
     extension (not a core trait impl) and consuming the Phase 2 ext-hook
     surface + typed handle system.

2. **`context/kits/cavekit-claude-code-hooks.md`** (NEW, optional — or
   merge into the above). Specifies the hook IPC protocol: `cc-hook`
   binary's JSON stdin format, control-socket protocol for forwarding
   hook POSTs to ark, permission-response reverse channel. This is the
   substance of what was in `cavekit-hook-ipc.md` but claude-code-scoped
   and living under the extension.

The 2 orchestrator kits (`cavekit-orchestrator-cavekit.md`,
`cavekit-orchestrator-claude-code.md`) stay deleted — they assumed a core
Orchestrator trait which is dying in soul Phase 5. No rehoming.

### Stays deleted

- `cavekit-architecture.md` (superseded by soul)
- `cavekit-orchestrator-claude-code.md`
- `cavekit-orchestrator-cavekit.md`
- ACP-related code (`crates/acp-client/`, ACP ops, ACP scene primitives)

---

## Scene kit 2026-04-18 revisions — keep as-is

These edits stand unchanged regardless of the pivot:

- `group` → `stack` naming (matches zellij).
- `stack @h { <view> }` primitive un-deferred.
- `Pane<V: View>` + `Stack<V: View>` parametric types.
- `HandleKind` narrows to `{Tab, Pane, Stack}`; `Command`/`Plugin` retire.
- Marker traits `View` / `CommandView` / `ZellijView` gate affordances.
- View alias inside `pane`/`stack` braces DECLARES the view type.
- Handle-typed view attrs + compile-time validation.
- `spawn_into @stack`, `clear @stack` ops.

Full rationale in the changelog section of `cavekit-scene.md`.

The claude-code extension will use all of these (typed handles for its
views, stack for its subagent fan-out, view-alias declarations in scene).

---

## Soul Phase 2 gap analysis (from earlier in this session)

Parent soul kit Phase 2 is a 4-bullet paragraph. Gaps identified:

1. **Hook list incomplete vs. ext consumers' needs:**
   - Missing `register_intents` (as a boot-time hook; distinct from the
     existing RPC `intent_register` on `ArkExtension`).
   - Missing view-registration surface (compile-time derive path for
     in-proc + RPC path for subprocess).
   - Missing config-schema registration (how does ext declare its TOML
     section to figment).
   - Missing scene-reload-gate registration.

2. **`permission_dispatcher` should be dropped** from the list — ACP is
   gone per Phase 3.

3. **Typed-handle runtime API not specified.** Scene R17 describes
   `Pane<V>` / `Stack<V>` semantics but where `StackHandle::spawn_pane`,
   `PaneHandle::replace_view`, `emit`, and compile-time handle-type
   resolution live in code is undefined. Which crate owns them, how
   subprocess extensions call them over RPC, how hot-reload invalidates
   them.

4. **Derive-macro spec missing.** `ark-ext-derive` needs to grow to
   support `#[derive(View)]`, marker-trait detection
   (`CommandView`/`ZellijView`), handle-kind SHAPE fields, typed view-attr
   codegen. Not in any current kit.

5. **Fan-in wiring not specified.** How `ark list` assembles columns from
   N exts; how `ark doctor` iterates; how figment layers ext-declared
   TOML sections; each needs a contract.

6. **Intent-registration double-path unreconciled.** Trait has
   `intent_register` (RPC). Scene R17 has `#[ark::intent]` (derive +
   `inventory::submit!`). Phase 2 adds `register_intents` (hook). Three
   paths, one ark-side registry. Need one clear story with tests.

7. **Back-compat policy unspecified.** Soul mentions "single minor version
   bump" — fine, but subprocess exts of older versions need to handle
   `method_not_found` on new methods gracefully. Test matrix + handshake
   version-gating not specified.

8. **Stub-ext test harness not specified.** Soul says "integration tests
   with a stub in-proc extension" — one bullet. Need what the stub covers
   + how it's reusable for claude-code and pi tests.

### Proposed Phase 2 decomposition

Mirror Phase 1's shape — 6 sub-kits + overview, ~30–35 requirements:

```
cavekit-soul-phase-2-overview.md
  # domain table + dep order

cavekit-soul-phase-2-supervisor-hooks.md       (~6 R)
  # Methods ark calls on ext:
  #   on_session_start, on_session_end,
  #   scene_compile_hook, doctor_checks,
  #   list_columns, control_verbs.
  # Drop permission_dispatcher (ACP gone).

cavekit-soul-phase-2-ext-registrations.md      (~6 R)
  # Registrations ark collects from ext at boot:
  #   register_intents, register_views,
  #   register_config_sections, register_reload_gates.
  # Compiled-in derive path + subprocess RPC path
  # reconciled — one ark-side registry.

cavekit-soul-phase-2-view-runtime.md           (~7 R)
  # Pane<V> / Stack<V> types + method surface.
  # Compile-time handle-type resolution (view-table
  # built at scene compile).
  # Reconciler spawn_pane / close integration.
  # RPC projection for subprocess exts.
  # Hot-reload invalidation.

cavekit-soul-phase-2-derive-macros.md          (~5 R)
  # ark-ext-derive grows:
  #   #[derive(View)]
  #   CommandView/ZellijView marker detection
  #   facet SHAPE handle-typed fields
  #   inventory::submit! for ext + intent + view
  # Compile-time signature-to-registry code-gen.

cavekit-soul-phase-2-fan-in.md                 (~5 R)
  # ark list column assembly.
  # ark doctor check runner.
  # figment ext-sections layering.
  # scene-compile view-type validator.
  # reload-gate dispatcher.

cavekit-soul-phase-2-tests.md                  (~3 R)
  # Stub-in-proc-ext test harness.
  # Subprocess back-compat matrix.
  # view-type-mismatch compile-error goldens.
```

Parent `cavekit-soul.md` Phase 2 paragraph should shrink to a pointer at
the overview file (mirror how Phase 1 currently works).

---

## Dependencies between the work streams

```
soul Phase 1 (in flight, other agent)
  │
  └─► soul Phase 2 (NEEDS DECOMPOSITION)
        │
        ├─► scene 2026-04-18 code landing
        │     (typed handles + stack; requires Phase 2 view-runtime)
        │
        └─► soul Phase 3 (ACP delete) + Phase 4 (cavekit delete + claude-code ext)
              │
              └─► claude-code extension (v0.1 target)
                    │
                    └─► soul Phase 5 (final trait cleanup)
                          │
                          └─► pi extension family (v0.2)
```

---

## Near-term receiver tasks (ordered)

1. **Revert kit edits per the "Reversals" list above.** No new design
   decisions required; all mechanical.
2. **Decompose soul Phase 2** into the 6 sub-kits per proposed layout.
   Follow `cavekit-soul-phase-1-*.md` files for format.
3. **Write `cavekit-claude-code-ext.md`** (new). Port the pi kit's
   structural template (R-numbered; view-centric coordination with typed
   handles); swap the bridge mechanism (TS ext → `cc-hook` binary +
   transcript watcher); swap events (pi.* → claude-code.*); drop the
   runtime-tool-injection surface (no pi-control equivalent in v0.1;
   document MCP-server idea as a future option).
4. **Update `cavekit-pi.md` status:** DRAFT → DEFERRED; add a v0.2
   positioning note at top; preserve all content.
5. **Update parent `cavekit-soul.md` Phase 4** to reflect the revised
   claude-code-as-ext plan (not "delete outright").

Soul Phase 1 (other agent) can keep running in parallel; Phase 2
decomposition is independent. Do NOT edit the Phase 1 sub-kits — leave
them to the running agent.

---

## Open questions for next session

- **MCP-based control surface for claude-code.** Does the receiver design
  it now (so claude-code ext is v0.1-complete) or defer to v0.2 alongside
  pi-control? My take: defer; claude-code without runtime tool injection
  is still a big win, and MCP-server-shape is a separate design surface
  worth its own session.
- **Stack expansion policy** for claude-code subagents. Same question I
  flagged for pi-subagents: user-controlled (zellij default) vs. view-
  decides vs. scene-attr. I'd default to zellij-default.
- **Auto-approve-safe tool policy.** `read_only_tools = ["Read", "Glob",
  "Grep", "WebFetch", "WebSearch"]` was the old default. Still correct?
  Is `WebSearch` read-only enough to auto-approve? Revisit during kit
  write.
- **How does the claude-code extension install its hook binary?** The hook
  binary (`cc-hook`) needs to be on the user's PATH or registered in
  `~/.claude/settings.json` hooks config. Ark's installer should handle
  this; surface via doctor + `ark ext claude-code install-hooks` verb.

---

## Inventory of code the claude-code extension can salvage

Some of what was deleted today is still in git. Recoverable for the
extension implementation:

- `crates/hook/` — was `ark-hook`. Whole crate is claude-code hook glue.
  Restore as `extensions/claude-code/bin/cc-hook/`.
- `crates/orchestrators/claude-code/` — passthrough orchestrator logic
  that watched claude's transcript. Restore as reference for the ext's
  transcript watcher.
- `crates/types/src/permission.rs` — `READ_ONLY_TOOLS` + `PermissionPolicy`
  + `POLICY_FILE_NAME`. Restore into `extensions/claude-code/src/permission.rs`.
- `crates/hook/src/event.rs` — `HookEvent` enum with the six Claude Code
  hook names. Still accurate; restore into ext.
- `crates/hook/src/payload.rs` — hook-payload-to-event translation. Still
  accurate; restore into ext.

`git log --all` should have these at the pre-2026-04-18-scope-cut commit.
`git show <sha>:crates/hook/src/lib.rs` etc. recover file content.

---

## End of handoff

This document is self-contained. The receiving agent should start by
reading it end-to-end, then execute the "Near-term receiver tasks" list.
Any question not covered above is a legitimate design decision to raise
with the user — don't invent an answer silently.
