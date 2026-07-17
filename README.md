# loti

[![CI](https://github.com/nivox/loti/actions/workflows/ci.yml/badge.svg)](https://github.com/nivox/loti/actions/workflows/ci.yml)

**loti** (LOcal TIckets) — a local, markdown-backed ticket tracker driven
entirely through the `loti` CLI.

> [!WARNING]
> **This project was developed by AI.** The code was written by an AI agent
> against a specific, structured specification authored by a human (see
> [`docs/specs/`](docs/specs/)). Its **code has not been thoroughly reviewed by
> hand.** It is exercised by an automated test suite, but no line-by-line human
> audit has been performed. If relying on unaudited AI-written code is a problem
> for your use case, please refrain from using this tool.

## What is this?

`loti` is a ticket tracker that lives **inside your repository** as plain
markdown files and is driven **entirely through a CLI** — there is no server, no
database, and no web UI. It is built for two audiences sharing one store:

- **AI agents** that plan and execute work autonomously and need somewhere
  durable to record epics, tickets, status, and an attributed audit trail.
- **One human** monitoring and steering that work, who can read the store as
  ordinary markdown (or through the same CLI) at any time.

Work is organised as **epics** (top-level units of work), **tickets** (slices of
an epic), and **subtickets** (finer breakdowns, nestable to any depth). Every
node has a state (`to-do`, `in-progress`, `blocked`, `done`, `closed`), and can
carry labels, comments, and file attachments. Because the store is just
greppable markdown committed alongside your code, the plan and its history stay
with the project.

The behaviour is fully specified in [`docs/specs/`](docs/specs/):
[`core-spec.md`](docs/specs/core-spec.md) (model, on-disk format, concurrency,
versioning) and [`cli-spec.md`](docs/specs/cli-spec.md) (command surface,
output, filtering, skill/help).

## Tutorial (for humans)

Get the binary (see [Nix flake](#nix-flake) below, or `cargo build`), then:

**1. Create a store** in your project — a git-like upward search finds it from
any subdirectory afterwards:

```sh
loti init
# → loti: initialised a store at /path/to/project
```

**2. Create an epic** (the top-level unit of work). A longer body is read from
stdin or `--file`; here we give none:

```sh
loti epic create website-redesign \
  --name "Website redesign" \
  --summary "Refresh the marketing site" < /dev/null
```

**3. Add tickets** to the epic (add `--parent <epic>/<n>` to make a subticket):

```sh
loti ticket create website-redesign --name "Audit current pages" --summary "Inventory existing content" < /dev/null
loti ticket create website-redesign --name "Design new layout"   --summary "Wireframes and mockups"       < /dev/null
```

**4. Drive status** as work moves. Status is set-only; read it back with `show`:

```sh
loti ticket status website-redesign/1 --in-progress
# → loti: ticket website-redesign/1 (Audit current pages) is now in-progress

loti ticket status website-redesign/1 --blocked --reason "waiting on brand assets"
loti ticket status website-redesign/1 --done            # allowed once all descendants are terminal
```

**5. Annotate and attribute.** Attribution is carried by comments (`-u` for the
human, `-a <name>` for a named agent); labels and file assets organise and
evidence work:

```sh
loti ticket label   add     website-redesign/2 frontend design
echo "Kicked off the wireframes." | loti ticket comment add website-redesign/2 -u
loti ticket asset   add     website-redesign/2 --file ./mockup.png --description "First pass"
```

**6. Read and report:**

```sh
loti ticket list website-redesign     # indented tree + a per-status progress footer
loti ticket show website-redesign/1   # one node (add --json for machine output)
loti epic  list                       # the roster of epics

loti ticket list website-redesign --status blocked           # filter by state
loti ticket list website-redesign --label frontend           # filter by label
loti ticket list website-redesign --match "wireframe"         # regex over name/summary/body
```

`loti ticket list` prints something like:

```
website-redesign/1 Audit current pages (in-progress)
website-redesign/2 Design new layout (to-do)
────────────────────────────────────────────────────
2 tickets · 1 to-do · 1 in-progress
```

Every read supports `--json` (the canonical form) so scripts and tools can
consume it. Run `loti --help-full` for the complete, annotated command surface.

## Using loti with AI agents

`loti` ships its own agent instructions. Running `loti skill` prints a static,
hand-authored **SKILL.md** — concepts, workflow, and gotchas — that teaches an
agent to drive the tracker correctly and points it at `loti --help-full` for the
exact command surface.

**Install the skill.** Ask your coding agent to install it, or do it yourself by
writing the skill where your agent framework discovers skills. For example:

```sh
# Save the skill so an agent can load it (adapt the path to your framework)
loti skill > SKILL.md
```

A prompt that installs and then delegates autonomously looks like:

> Install the `loti` skill by running `loti skill` and saving its output as a
> skill your tools can load. Then use `loti` to track your work on this task:
> create an epic for the feature, break it into tickets (and subtickets where
> useful), keep each ticket's status current as you work (`in-progress`,
> `blocked` with a reason, `done`), and leave a comment attributing each
> meaningful change with `-a <your-name>`. Read state back with
> `loti ticket list <epic>` and `loti ticket show <ref>` — never hand-edit the
> store files.

Once the skill is loaded, the agent can operate the tracker on its own: it plans
by creating epics and tickets, records progress through status transitions, and
leaves an attributed trail you can review at any time with the same commands —
or by reading the markdown directly. The single rule the skill enforces is that
**every change goes through the CLI**; the files are for reading, never for
hand-editing (the CLI is what guarantees numbering, state transitions,
attribution, and safe concurrent writes).

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
