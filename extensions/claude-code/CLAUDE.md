# extensions/claude-code

Claude Code as a first-class ark extension. Single-crate extension with a
sub-binary (`bin/cc-hook/`) that bridges Claude Code's hook system to
ark's event bus via NDJSON over a unix socket.

Implements:
- cavekit-claude-code.md R1..R13 + R5b

Build site: build-site-claude-code-ext.md (48/48, closed 2026-04-18)
Impl tracking: context/impl/impl-claude-code-ext.md
