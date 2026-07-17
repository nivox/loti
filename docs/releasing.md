# Releasing loti

## Versioning model

`Cargo.toml` (`workspace.package.version`) is the single source of truth for the
version. It flows, baked at compile time, to both `loti --version` (via clap) and
the Nix flake package (via `builtins.fromTOML`).

**Main always carries the next release with a `-dev` pre-release suffix.** For
example, while the last release was `0.1.0`, main reads `0.2.0-dev`. Because
semver orders `0.2.0-dev` before `0.2.0`, anyone building the flake from main
gets a version that is visibly in-development and sorts ahead of the tag it will
become. A release is the single commit where that suffix is stripped.

`Cargo.lock` records the same version for the two workspace crates, so it is
bumped in lockstep with `Cargo.toml`; otherwise `cargo build --locked` (used by
the release workflow) would reject the tree.

## The cycle

```
main:            0.2.0-dev     in-development
release commit:  0.2.0         tagged v0.2.0  → release workflow builds binaries
main resumes:    0.3.0-dev     next in-development version
```

## Cutting a release

Run the release script from a clean `main` that is up to date with the remote:

```sh
scripts/release.sh                 # release the -dev version, resume on next minor
scripts/release.sh --next 0.2.1    # choose the next in-development version explicitly
scripts/release.sh --push          # also push the branch and tag when done
```

The script:

1. reads the current `X.Y.Z-dev` version and strips the suffix to `X.Y.Z`;
2. updates `Cargo.toml` + `Cargo.lock`, commits `release: vX.Y.Z`, and tags
   `vX.Y.Z`;
3. bumps main to the next in-development version (default: next minor) and
   commits it;
4. pushes with `--push`, or prints the push command otherwise.

Pushing the `vX.Y.Z` tag triggers `.github/workflows/release.yml`, which builds
a static binary per platform and attaches it to the GitHub release. That
workflow refuses to build a tag whose Cargo version still carries `-dev` or does
not match the tag, so a mistagged or unbumped release fails fast instead of
shipping the wrong version.

## Doing it by hand

If you cannot run the script, the equivalent manual steps are:

1. On a clean `main`, edit `workspace.package.version` in `Cargo.toml` from
   `X.Y.Z-dev` to `X.Y.Z`, and update both `loti-core`/`loti-cli` entries in
   `Cargo.lock` to match.
2. `git commit -am "release: vX.Y.Z"` then `git tag -a vX.Y.Z -m "loti vX.Y.Z"`.
3. Bump `Cargo.toml`/`Cargo.lock` to the next `A.B.C-dev` and
   `git commit -am "chore: begin A.B.C-dev"`.
4. `git push origin main && git push origin vX.Y.Z`.
