# Joskus integration contract

Joskus drives the agent session. Toimija supplies a verified snapshot of the
local repository state.

## Contract at a glance

| Need | Interface |
| --- | --- |
| Refresh the packet | `toimija current --write` |
| Verify before reading | `toimija current --verify --json` |
| Human-readable context | `.toimija/current.md` |
| Machine-readable context | `.toimija/current.json` |
| Wrapped-session packet | `.toimija/sessions/<id>/current.{md,json}` |

The JSON packet is the machine contract. Markdown is a bounded presentation of
the same artifact and may omit optional sections to meet its token budget.

## Consumer flow

```text
toimija current --write
        │
        ▼
toimija current --verify --json
        │
        ├── exit 0 / status "verified" ──► read current.json
        │
        └── exit 1 / refusal status ─────► do not use the packet
```

Verification selects the ambient session packet when `TOIMIJA_SESSION` is set,
and the repo-level packet otherwise. Pass `--packet <path>` to select a packet
explicitly.

A successful response includes the selected `packet_path`, consistency
`algorithm`, `token`, and `artifact_digest`:

```json
{
  "status": "verified",
  "packet_path": "/repo/.toimija/sessions/123-456/current.json",
  "algorithm": "toimija-consistency-v3",
  "token": "sha256:...",
  "artifact_digest": "sha256:..."
}
```

Any non-zero result is a refusal to trust the packet. Stable status values are:

- `missing_packet`
- `invalid_packet`
- `missing_consistency`
- `not_comparable` (the packet's token is of an earlier version: neither fresh
  nor stale; refresh it)
- `unsupported_consistency_algorithm`
- `stale_packet`
- `sauma_digest_mismatch`
- `unsafe_git_operation`
- `unverifiable_repository`
- `io_error`

## Packet guarantees

Each persisted Markdown/JSON pair shares an `artifact_id`. JSON is authoritative
if an interrupted refresh leaves mismatched IDs.

The packet includes:

- Git identity and work state: branch, HEAD, merge base, index, worktree,
  untracked files, conflicts, and in-flight operations;
- changed surface, compiled workset, read order, configured boundaries, and
  validation plan;
- collection coverage and evidence-backed relevance hints;
- a `toimija-consistency-v3` token over the current Git work state;
- a sauma envelope binding the artifact to its provenance;
- on session packets, session identity, declared intent and scope, fixed entry
  branch/HEAD, drift, and an execution context carrying the session's
  repository point (`session.execution.repository`).

The consistency token is content-based. File mtimes do not affect it; same-size
content changes that `git status` lists do. Generated state under `.toimija/`,
`.teko/` and `.sykli/` is excluded, except `.toimija/repo.yaml`, which is
repository policy: exactly what tree-identity.v1 excludes. The token is a
freshness check, not an identity; a repository point's id is derived from its
tree, so equal point ids mean equal trees (README, "Repository points").

Coverage values mean:

- `available`: the observation succeeded, even when its result is empty;
- `degraded`: only partial or legacy evidence is available;
- `failed`: collection produced no usable claim.

Consumers must not interpret an empty result with failed coverage as evidence
that nothing exists.

## Compatibility rules

- New optional fields may be added without a schema-version bump.
- Consumers must ignore unknown fields and tolerate unknown sauma provenance
  edge kinds.
- Unix paths are JSON strings when valid UTF-8 and `{ "bytes_hex": "..." }`
  otherwise. Consumers must accept both forms.
- Changed-file entries may include `original_path` and `similarity` for renames
  and copies.
- Do not materialize additive defaults before verifying the sauma digest; verify
  the raw JSON value first.

## Wrapped sessions

`toimija run` creates a per-session packet, keeps it fresh while the child is
running, and exits with the child's status. Explicit `--intent` is required;
`--intent unknown` is the deliberate escape hatch.

Known agents (`claude`, `codex`, and `gemini`) receive the packet and task in
their launch prompt. Other commands receive only the environment handshake:

| Variable | Meaning |
| --- | --- |
| `TOIMIJA_SESSION` | Current session ID |
| `TOIMIJA_LIVE` | Authoritative session Markdown packet |
| `TOIMIJA_LIVE_JSON` | Authoritative session JSON packet |
| `TOIMIJA_CURRENT_MD` | Alias of `TOIMIJA_LIVE` |
| `TOIMIJA_CURRENT_JSON` | Alias of `TOIMIJA_LIVE_JSON` |
| `TOIMIJA_REPO_ROOT` | Repository root |
| `TOIMIJA_RUN_ID` | Alias of `TOIMIJA_SESSION` |
| `TOIMIJA_REPO_LIVE` | Repo-level latest-summary packet |
| `FALSE_SYSTEMS_REPOSITORY_POINT_PATH` | The session's current repository point (false-agent-protocol `RepositoryPointEnvelope`) |

Toimija owns packet compilation, freshness, launch delivery, and local
provenance. It does not call model providers, parse model output, apply patches,
or execute model-requested validation.

For the full packet anatomy and command reference, see
[reference.md](reference.md).
