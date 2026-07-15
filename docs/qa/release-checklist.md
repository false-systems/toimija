# Release-candidate checklist

## Code and contracts

- [ ] cargo fmt --check
- [ ] cargo clippy --all-targets -- -D warnings
- [ ] cargo test
- [ ] Public command/flag inventory matches --help
- [ ] Exit-code contract reviewed
- [ ] JSON schemas and stdout/stderr separation reviewed
- [ ] Unsupported repository shapes documented

## P0 correctness

- [ ] Staged/unstaged/partial/untracked/conflict matrix is green
- [ ] All supported interrupted Git operations are detected and safety-gated
- [ ] Freshness mutation matrix is green
- [ ] Concurrent refresh/read test is green
- [ ] Atomic-write failure preserves the previous valid packet
- [ ] Hostile path and symlink tests are green

## Tests and security

- [ ] Unit, component, CLI, system, E2E, regression, and property suites pass
- [ ] Bounded parser fuzz corpus passes with no panic
- [ ] No shell execution occurs from repository-controlled text
- [ ] ANSI/path/remote/config secret tests pass
- [ ] No network or paid model is required in default CI
- [ ] Flaky tests have an owner and are not hidden by retries

## Platform and packaging

- [ ] Supported Linux/macOS/Windows policy matches CI
- [ ] Release-mode binary smoke test passes from a clean directory
- [ ] Archive layout, executable bit, version, license, and checksum verified
- [ ] Cargo installation path tested if advertised
- [ ] Minimum Git/Rust versions tested

## Performance and compatibility

- [ ] Tiny through large generated profiles measured
- [ ] Watch idle/update and refresh concurrency measured
- [ ] RSS, file descriptors, process count, and output bytes recorded
- [ ] No unexplained p95 regression beyond policy
- [ ] Prior generated-state fixtures pass the compatibility policy
- [ ] Documentation examples execute

## Sign-off

- [ ] QA lead
- [ ] Maintainer
- [ ] Security review
- [ ] Release artifact owner
- [ ] Known limitations and next review date recorded
