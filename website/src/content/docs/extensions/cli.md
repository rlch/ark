---
title: "Extension CLI"
description: "ark ext add, remove, update"
---

The `ark ext` subcommand manages extension installation, inspection, and removal.

## `ark ext add`

Install an extension from a source.

```bash
ark ext add github:user/repo       # from GitHub
ark ext add path:./my-ext          # from a local directory
ark ext add url:https://example.com/ext.tar.gz  # from a URL
```

On install, ark reads the extension manifest, displays the requested [capabilities](/extensions/capabilities/), and prompts for approval. The extension is installed to `~/.local/share/ark/extensions/<name>/`.

**Flags:**

| Flag | Description |
|------|-------------|
| `--yes` | Auto-approve capabilities (skip prompt) |
| `--project` | Install to `.ark/extensions/` instead of user-global |

## `ark ext remove`

Uninstall an extension.

```bash
ark ext remove my-ext
```

Removes the extension directory and revokes its capability grants. Active sessions using the extension are not affected until the next session start.

## `ark ext update`

Update one or all extensions.

```bash
ark ext update my-ext    # update a specific extension
ark ext update           # update all installed extensions
```

If the update introduces new capabilities, ark re-prompts for approval (unless `--yes` is passed).

**Flags:**

| Flag | Description |
|------|-------------|
| `--yes` | Auto-approve new capabilities |
| `--dry-run` | Show what would change without applying |

## `ark ext list`

List installed extensions.

```bash
ark ext list
```

Output includes name, version, delivery mode, and declared capabilities for each extension.

## `ark ext info`

Show details about an installed extension.

```bash
ark ext info claude-code
```

Displays the full manifest: version, intents, events, config schema, capabilities, and available scene fragments.

## `ark ext inspect`

Inspect an extension artifact on disk without installing it.

```bash
ark ext inspect path/to/extension.wasm
ark ext inspect path/to/extension-dir/
```

Reads the manifest (from the `ark.metadata` wasm section or `extension.kdl` file) and prints the parsed contents. Validates against size limits and capability vocabulary. Useful for reviewing third-party extensions before installation.

## `ark ext trust`

Grant or display capability trust for an installed extension.

```bash
ark ext trust my-ext          # show current trust grants
ark ext trust my-ext --grant  # interactively re-approve capabilities
```

Use this to audit or re-approve capabilities for an already-installed extension without reinstalling it.
