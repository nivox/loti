#!/usr/bin/env bash
# Release driver for loti.
#
# Invariant this script enforces: main always carries the NEXT release with a
# `-dev` pre-release suffix. A release is a single tagged commit whose version
# has the suffix stripped; immediately after, main moves on to the next
# in-development version (again suffixed). The two version-bearing files stay in
# lockstep: Cargo.toml (authoritative) and the workspace entries in Cargo.lock.
#
# Usage:
#   scripts/release.sh [--next X.Y.Z] [--push] [--yes]
#
#   --next X.Y.Z  Version main resumes on after the release (a `-dev` suffix is
#                 added). Default: bump the minor of the release version.
#   --push        Push the release commit, tag, and follow-up bump when done.
#   --yes         Skip the confirmation prompt.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

next_release_dev=""
do_push=0
assume_yes=0
while [ $# -gt 0 ]; do
  case "$1" in
    --next) next_release_dev="${2:?--next needs a version}"; shift 2 ;;
    --push) do_push=1; shift ;;
    --yes|-y) assume_yes=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

die() { echo "release: $*" >&2; exit 1; }

# Read the authoritative workspace version (the only top-level `version = "…"`
# in Cargo.toml; members inherit via `version.workspace = true`).
read_version() { sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1; }

# Keep Cargo.toml and both workspace entries in Cargo.lock in lockstep so
# `cargo build --locked` (used by CI) stays valid without a network fetch.
set_version() {
  local new="$1"
  sed -i.bak 's/^version = ".*"/version = "'"$new"'"/' Cargo.toml && rm -f Cargo.toml.bak
  local pkg
  for pkg in loti-core loti-cli; do
    awk -v pkg="$pkg" -v new="$new" '
      $0 == "name = \"" pkg "\"" { print; getline; sub(/version = ".*"/, "version = \"" new "\""); print; next }
      { print }
    ' Cargo.lock > Cargo.lock.tmp && mv Cargo.lock.tmp Cargo.lock
  done
}

# --- preconditions -----------------------------------------------------------
[ -z "$(git status --porcelain)" ] || die "working tree is not clean; commit or stash first."
branch="$(git rev-parse --abbrev-ref HEAD)"
[ "$branch" = "main" ] || echo "release: warning — on branch '$branch', not 'main'." >&2

current="$(read_version)"
[ -n "$current" ] || die "could not read version from Cargo.toml."
case "$current" in
  *-dev) ;;
  *) die "Cargo version is '$current' (no -dev suffix); main should carry an in-development version." ;;
esac

release="${current%-dev}"
git rev-parse -q --verify "refs/tags/v$release" >/dev/null 2>&1 \
  && die "tag v$release already exists."

if [ -z "$next_release_dev" ]; then
  # Default: bump the minor, reset patch (0.1.0 -> 0.2.0).
  IFS=. read -r maj min _pat <<<"$release"
  next_release_dev="$maj.$((min + 1)).0"
fi
next_dev="$next_release_dev-dev"

# --- confirm -----------------------------------------------------------------
cat <<EOF
About to:
  1. set version $current -> $release, commit + tag v$release
  2. set version $release -> $next_dev, commit (main resumes in-development)
EOF
[ "$do_push" -eq 1 ] && echo "  3. push main and tag v$release" || echo "  (not pushing; use --push to push)"

if [ "$assume_yes" -ne 1 ]; then
  printf 'Proceed? [y/N] '
  read -r reply
  case "$reply" in y|Y|yes) ;; *) die "aborted." ;; esac
fi

# --- release commit + tag ----------------------------------------------------
set_version "$release"
git add Cargo.toml Cargo.lock
git commit -m "release: v$release"
git tag -a "v$release" -m "loti v$release"

# --- resume in-development ---------------------------------------------------
set_version "$next_dev"
git add Cargo.toml Cargo.lock
git commit -m "chore: begin $next_dev"

echo "release: created commit + tag v$release and bumped main to $next_dev."

if [ "$do_push" -eq 1 ]; then
  git push origin "$branch"
  git push origin "v$release"
  echo "release: pushed $branch and tag v$release."
else
  echo "release: to publish, run:"
  echo "    git push origin $branch && git push origin v$release"
fi
