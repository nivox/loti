# loti

**loti** (LOcal TIckets) — a local, markdown-backed ticket tracker driven
entirely through the `loti` CLI.

## Nix flake

The repository ships a `flake.nix` that provides a dev shell plus a package
build. The Rust channel and components come from [`rust-toolchain.toml`](rust-toolchain.toml)
and the package version from [`Cargo.toml`](Cargo.toml), so both stay single-sourced.
Supported systems: `x86_64`/`aarch64` Linux and `aarch64` Darwin.

### Develop

Enter the dev shell (Rust, `clippy`, `rustfmt`, `rust-analyzer`, and the musl
static-build targets):

```sh
nix develop
```

Then the usual cargo workflow inside the shell:

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

### Build a release

Build the binary through the flake:

```sh
nix build            # or: nix build .#loti
./result/bin/loti --help
```

Or run it without building into the working tree:

```sh
nix run . -- --help
```

For a **single static binary** (Linux), build the musl target from inside the
dev shell — the `+crt-static` rustflags are preset:

```sh
nix develop --command \
  cargo build --release --target x86_64-unknown-linux-musl
# → target/x86_64-unknown-linux-musl/release/loti  (statically linked)
```

Swap `x86_64` for `aarch64` to target arm64.
