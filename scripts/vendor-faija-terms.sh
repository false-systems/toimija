#!/usr/bin/env bash
# Refresh the faija-terms crate Toimija carries.
#
#   scripts/vendor-faija-terms.sh <faija-checkout> [revision]
#
# Copies `crates/faija-terms/` out of faija at the named revision (a tag such
# as `v0.1.0`, or a commit), byte for byte, and rewrites
# `vendor/faija-terms.contract`. It never edits the crate: if the copy needs
# changing, the change belongs in faija.
#
# faija owns tree-identity.v1, the canonical procedure for the identity of a
# working State. Toimija runs that procedure from this copy rather than keeping
# its own. Carried rather than fetched, like sauma-canonical: faija is a private
# repository and publishes nothing, so a git dependency would cost every build a
# credential. There is still exactly one implementation, distributed rather than
# linked, and `tests/vendored_faija_terms.rs` recomputes the digest on every
# run, so an edit here fails the build and shows up as a diff touching
# `vendor/`.
#
# After a re-pin, read every call site against the new crate: a re-pin that
# only compiles can keep an old rule (see `src/snapshot.rs`, `TREE_ALGORITHM`).
set -euo pipefail
faija="${1:?usage: vendor-faija-terms.sh <faija-checkout> [revision]}"
revision="${2:-origin/main}"
root="$(cd "$(dirname "$0")/.." && pwd)"

full=$(git -C "$faija" rev-parse "$revision^{commit}")
tag=$(git -C "$faija" describe --tags --exact-match "$full" 2>/dev/null || echo untagged)

# Extract into scratch BEFORE touching the repository: a revision without the
# crate fails here and changes nothing.
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
git -C "$faija" archive "$full" crates/faija-terms/Cargo.toml crates/faija-terms/src \
  | tar -x -C "$scratch"
[ -s "$scratch/crates/faija-terms/Cargo.toml" ] && [ -d "$scratch/crates/faija-terms/src" ] \
  || { echo "vendor-faija-terms: $full lacks the crate; nothing was changed" >&2; exit 1; }

rm -rf "$root/vendor/faija-terms"
mkdir -p "$root/vendor"
cp -R "$scratch/crates/faija-terms" "$root/vendor/faija-terms"

# The digest `tests/vendored_faija_terms.rs` reproduces: one line per file,
# `<relative path>  <sha256>`, sorted by path, digested whole. A rename, a
# reorder or a truncation all change it.
listing=$(cd "$root/vendor/faija-terms" && find . -type f | sed 's|^\./||' | LC_ALL=C sort \
  | while IFS= read -r f; do printf '%s  %s\n' "$f" "$(shasum -a 256 "$f" | cut -d' ' -f1)"; done)
digest=$(printf '%s' "$listing" | shasum -a 256 | cut -d' ' -f1)
files=$(printf '%s\n' "$listing" | grep -c .)

cat > "$root/vendor/faija-terms.contract" <<EOF
# The faija-terms crate Toimija carries.
#
# faija owns tree-identity.v1: which namespaces a working State's identity
# leaves out, which configuration it forces in, and the git steps in their
# order. Toimija runs that procedure from this copy (src/snapshot.rs) rather
# than keeping a tree algorithm of its own.
#
# \`vendor/faija-terms/\` is byte-identical to \`crates/faija-terms/\` at
# \`faija-revision\`, and \`tests/vendored_faija_terms.rs\` recomputes
# \`tree-digest\` on every run, so an edit here fails the build. Not a security
# boundary: it makes drift visible as a diff touching \`vendor/\`.
#
# Refresh with \`scripts/vendor-faija-terms.sh <faija-checkout> <revision>\`,
# never by editing the crate in place.
faija-tag = $tag
faija-revision = $full
tree-digest = sha256:$digest
files = $files
EOF
echo "vendored faija-terms at $full ($tag): $files files, sha256:$digest"
