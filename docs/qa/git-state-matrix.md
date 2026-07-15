# Git-state matrix

Yes means tested through a public or component interface. Partial means a
nearby case exists but not the complete state. Gap means release qualification
has not established the behaviour.

| Git state | Detected | Human | JSON | Blocks readiness | Coverage | Notes |
|---|---|---|---|---|---|---|
| clean committed branch | Yes | Yes | Yes | No | Good | baseline |
| staged file | Yes | Yes | Yes | No | Good | separate field |
| unstaged file | Yes | Yes | Yes | No | Good | warning |
| untracked file | Yes | Yes | Yes | No | Good | warning |
| ignored file | Partial | Yes | Partial | No | Partial | token exclusion needs matrix |
| staged + unstaged same file | Yes | Yes | Yes | Yes | P0-01 | release-critical |
| partially staged hunk | Partial | Partial | Partial | Yes | Gap | real Git fixture needed |
| rename | Yes | Yes | Yes | No | Good | origin retained |
| copy | Partial | Partial | Partial | Yes | Gap | parser/path cases incomplete |
| delete/recreate | Partial | Partial | Partial | Yes | Gap | intent ambiguity |
| executable-bit change | Partial | Partial | Partial | Yes | Gap | token code exists |
| binary file | Partial | Partial | Partial | No | Gap | rendering/budget policy |
| Unicode path | Yes | Yes | Yes | No | Good | path codec |
| newline/ANSI path | Yes | Yes | Yes | Yes | Partial | security assertions needed |
| unborn branch | Yes | Yes | Yes | Yes | Good | no HEAD |
| detached HEAD | Yes | Yes | Yes | Yes | Good | risky warning |
| ahead/behind/diverged | Partial | Partial | Partial | Yes | Gap | no fetch claim allowed |
| no upstream | Yes | Yes | Yes | No | Good | explicit warning |
| merge in progress | Yes | Yes | Yes | Yes | P0-03 | other operations missing |
| rebase in progress | Yes | Partial | Partial | Yes | Partial | run gate coverage |
| cherry-pick in progress | Partial | Partial | Partial | Yes | Gap | operation matrix |
| revert in progress | Partial | Partial | Partial | Yes | Gap | operation matrix |
| bisect in progress | Partial | Partial | Partial | Yes | Gap | operation matrix |
| locked index | Partial | Partial | Partial | Yes | Gap | failure injection |
| main worktree | Yes | Yes | Yes | No | Partial | identity tests needed |
| linked worktree | Partial | Partial | Partial | Yes | Gap | repository/worktree split |
| bare repository | Unknown | Unknown | Unknown | Yes | Gap | unsupported until tested |
| submodule | Partial | Partial | Partial | Yes | Gap | gitlink semantics |
| shallow clone | Yes | Yes | Yes | Yes | Partial | merge-base warning |
| sparse checkout | Unknown | Unknown | Unknown | Yes | Gap | no completeness claim |
| partial/promisor clone | Unknown | Unknown | Unknown | Yes | Gap | unsupported |
| nested repository | Partial | Partial | Partial | Yes | Gap | discovery boundary |

Until Gap rows have an explicit supported/unsupported policy, the public
promise must remain limited to ordinary non-bare checkouts.
