# Joskus Contract

Toimija prepares the repo. Joskus drives the session.

Toimija owns repo truth: it reads Git state, compiles context, writes
`.toimija/current.md`, writes `.toimija/current.json`, and verifies the packet
against the live repository. Toimija does not talk to model providers, own
prompts, apply model patches, or run model-requested validation.

## Files

`.toimija/current.md` is the readable context packet.

`.toimija/current.json` is the machine-readable packet. On per-session packets
(written under `.toimija/sessions/<id>/` by `toimija run`) it also carries a
`session` block — `{ "id": "...", "intent": "..."|null, "scope": [...] }` —
additive per the contract's evolution rule; a harness infers the session's
declared intent from it. It includes:

```json
{
  "consistency": {
    "algorithm": "toimija-consistency-v2",
    "head": { "kind": "normal", "branch": "main", "oid": "..." },
    "index_fingerprint": "sha256:...",
    "worktree_fingerprint": "sha256:...",
    "untracked_fingerprint": "sha256:...",
    "git_operation": { "kind": "none" },
    "conflict_state": { "has_conflicts": false, "files": [] },
    "token": "sha256:..."
  }
}
```

The token proves freshness of the packet against the current Git working state.
v2 uses deterministic Git status classes, normalized paths, file type, file
mode, and a **SHA-256 content hash** of each changed and untracked file
(symlinks fingerprint their target). Content-based, not time-based: the same
bytes always fingerprint the same regardless of mtime, and different bytes
always differ — so a bare `touch` does not falsely invalidate the token, and a
same-size edit cannot slip through. (v1 used file size + mtime and is rejected
with `unsupported_consistency_algorithm`.) Ignored files and Toimija-generated
packet files do not affect the token.

## Commands

Refresh synchronously:

```bash
toimija current --write
```

Verify synchronously:

```bash
toimija current --verify --json
```

`--json` verification emits one of:

- `verified`
- `missing_packet`
- `invalid_packet`
- `missing_consistency`
- `unsupported_consistency_algorithm`
- `stale_packet`
- `unsafe_git_operation`
- `unverifiable_repository`
- `io_error`

Exit code `0` means verified. Exit code `1` means the packet is not verified.

## Run Wrapper

`toimija run -- <command> [args...]` refreshes the packet, launches the child
with normal stdio, watches while it runs, and exits with the child status. The
child receives optional discovery variables:

- `TOIMIJA_CURRENT_MD`
- `TOIMIJA_CURRENT_JSON`
- `TOIMIJA_REPO_ROOT`
- `TOIMIJA_RUN_ID`

Toimija does not inject prompts or parse model output.
