# Joskus Contract

Toimija compiles and verifies modeled local repo state. Joskus drives the session.

Toimija owns that local state packet: it reads Git state, compiles context, writes
`.toimija/current.md`, writes `.toimija/current.json`, and verifies the packet
against the live repository. Toimija does not talk to model providers, own
prompts, apply model patches, or run model-requested validation.

## Files

`.toimija/current.md` is the readable context packet.

`.toimija/current.json` is the machine-readable packet. On per-session packets
(written under `.toimija/sessions/<id>/` by `toimija run`) it also carries a
`session` block — `{ "id": "...", "intent": "..."|null, "scope": [...] }` —
additive per the contract's evolution rule; a harness infers the session's
declared intent from it. Persisted Markdown/JSON pairs share an `artifact_id`;
JSON is authoritative when an interrupted refresh leaves mismatched IDs. It includes:

```json
{
  "artifact_id": "...",
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

Path fields remain JSON strings when they are valid UTF-8. Unix paths that are
not valid UTF-8 use `{ "bytes_hex": "..." }`, preserving the exact bytes without
replacement characters. Consumers must accept both forms.
Changed-file objects may include `original_path` and `similarity` for Git
renames/copies. `git.conflict_entries` preserves stages 1/2/3 with their mode
and object ID while the existing `conflicted_files` list remains available.

Collection stages carry coverage (additive, R10): `git.branch_diff_coverage`
and `relevance.history_coverage` are `{ "status": "available"|"degraded"|
"failed", "reason": "...", "source": "..." }` objects, and `toimija sessions
--json` carries a top-level `coverage` for the marker scan. `available` means
the (possibly empty) evidence is a real observation; `failed` means the
collection did not deliver and no claim — positive or negative — may be derived
from the empty list next to it. Packets written before these fields existed
deserialize as `degraded` with reason `packet predates coverage reporting`.
Coverage describes the collection, not the repo state, so it never feeds the
consistency token.

The token proves freshness of the packet against the current Git working state.
v2 uses deterministic Git status classes, normalized paths, file type, file
mode, and a **SHA-256 content hash** of each changed and untracked file
(symlinks fingerprint their target). Content-based, not time-based: the same
bytes always fingerprint the same regardless of mtime, and different bytes
always differ — so a bare `touch` does not falsely invalidate the token, and a
same-size edit cannot slip through. (v1 used file size + mtime and is rejected
with `unsupported_consistency_algorithm`.) Ignored files and generated state
under `.toimija/` do not affect the token, except `.toimija/repo.yaml`, which is
repo policy and always counts.

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
