# ark

Zellij-native agent orchestration.

Spawns AI coding agents into dedicated zellij sessions with live diff, git status, and progress panes. Pluggable via two-layer adapter model:

- **Engines** extract structured signal from agent CLIs (`ClaudeCodeEngine` injects hooks, tails transcripts)
- **Orchestrators** encode workflow methodology (`CavekitOrchestrator`, `ClaudeCodeOrchestrator`)

Status: early development. See `context/kits/` for the spec, `context/plans/` for the build site.

## License

MIT
