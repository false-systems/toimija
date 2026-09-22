#!/usr/bin/env bash
# Refresh the canonical crate Toimija carries.
#
#   scripts/vendor-sauma-canonical.sh <sauma-checkout> [revision]
#
# Copies `sauma-canonical/` out of sauma at the named revision, byte for byte,
# and rewrites `vendor/sauma-canonical.contract`. It never edits the crate: if
# the copy needs changing, the change belongs in sauma.
#
# Carried rather than fetched, for the reason taso carries selko-canonical:
# sauma is a private repository and publishes nothing, so a git dependency
# would cost every build a credential. The monopoly is unchanged — there is
# still exactly one implementation, distributed rather than linked — and
# `tests/vendored_canonical.rs` recomputes the digest on every run, so an edit
# here fails the build and shows up as a diff touching `vendor/`.
set -euo pipefail
sauma="${1:?usage: vendor-sauma-canonical.sh <sauma-checkout> [revision]}"
revision="${2:-origin/main}"
root="$(cd "$(dirname "$0")/.." && pwd)"

full=$(git -C "$sauma" rev-parse "$revision^{commit}")
tag=$(git -C "$sauma" describe --tags --exact-match "$full" 2>/dev/null || echo untagged)

# Fetch and check into scratch BEFORE touching the repository: a revision
# without the crate fails here and changes nothing.
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
git -C "$sauma" archive "$full" sauma-canonical/Cargo.toml sauma-canonical/src fixtures \
  | tar -x -C "$scratch"
[ -s "$scratch/sauma-canonical/Cargo.toml" ] && [ -d "$scratch/sauma-canonical/src" ] \
  || { echo "vendor-sauma-canonical: $full lacks the crate; nothing was changed" >&2; exit 1; }

rm -rf "$root/vendor/sauma-canonical"
mkdir -p "$root/vendor"
cp -R "$scratch/sauma-canonical" "$root/vendor/sauma-canonical"

# The digest construction `tests/vendored_canonical.rs` reproduces: one line
# per file, `<relative path>  <sha256>`, sorted by path, digested whole. A
# rename, a reorder or a truncation all change it.
listing=$(cd "$root/vendor/sauma-canonical" && find . -type f | sed 's|^\./||' | LC_ALL=C sort \
  | while IFS= read -r f; do printf '%s  %s\n' "$f" "$(shasum -a 256 "$f" | cut -d' ' -f1)"; done)
digest=$(printf '%s' "$listing" | shasum -a 256 | cut -d' ' -f1)
files=$(printf '%s\n' "$listing" | grep -c .)

cat > "$root/vendor/sauma-canonical.contract" <<EOF
# The canonical crate Toimija carries.
#
# Sauma owns the envelope, its canonical byte string and every digest over it.
# Toimija holds that implementation here rather than fetching it from a private
# repository on every build. There is still exactly one implementation; it is
# distributed rather than linked.
#
# What keeps it honest: \`vendor/sauma-canonical/\` is byte-identical to
# \`sauma-canonical/\` at \`sauma-revision\`, and \`tests/vendored_canonical.rs\`
# recomputes \`tree-digest\` on every run, so an edit here fails the build.
#
# This is not a security boundary — the same commit could change both the crate
# and this file. It makes drift visible: what a reviewer sees is a diff that
# touches \`vendor/\`, which is the point.
#
# Refresh with \`scripts/vendor-sauma-canonical.sh <sauma-checkout> <revision>\`,
# never by editing the crate in place.
sauma-tag = $tag
sauma-revision = $full
tree-digest = sha256:$digest
files = $files
EOF
echo "vendored sauma-canonical at $full ($tag): $files files, sha256:$digest"
