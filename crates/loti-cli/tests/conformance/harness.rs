//! Black-box test harness: drives the compiled `loti` binary as a subprocess
//! against a throwaway store, so these checks exercise the shipping CLI exactly
//! as a user (or agent) would, never the crate internals.
//!
//! Every test gets its own temporary store. The container is the only directory
//! loti owns — it holds `meta` and every epic dir — and the two isolation
//! strategies place it differently:
//!   * `Store::new()` — drives every command with `--root <tempdir>`, so the
//!     container is the tempdir itself and `meta` lives at `<tempdir>/meta`.
//!     This is the hermetic default; discovery and the current directory are
//!     never touched. Epic-relative paths like `path("e/1.md")` stay valid.
//!   * `Store::new_discovered()` — for the few checks that must observe
//!     discovery / `.loti.conf`. Commands run with the current dir set to a
//!     tempdir, so the container is the discovered `<tempdir>/.loti`; `meta`
//!     lives at `<tempdir>/.loti/meta` and store contents under `.loti/`.
//!
//! Setup never invokes `loti init`: every store here is created by writing the
//! store metadata directly (matching what `loti init` writes), so a test's
//! store is ready without depending on the current directory or discovery. The
//! `init` verb's own behaviour is exercised separately in the storage suite.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use tempfile::TempDir;

/// The metadata a fresh store carries, matching what `loti init` writes for the
/// version this binary understands. Kept as a literal so the harness does not
/// depend on the core crate.
const CURRENT_META: &str = "format-version = \"1.2\"\n";

/// A throwaway store rooted in a tempdir, plus a builder for binary invocations
/// against it.
pub struct Store {
    dir: TempDir,
    root: PathBuf,
    /// When true, commands are driven by setting the child's current dir to the
    /// root (exercising discovery) rather than passing `--root`.
    via_discovery: bool,
}

impl Store {
    /// A store driven via `--root <tempdir>`: the container is the tempdir, so
    /// `meta` is written at `<tempdir>/meta`.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("make tempdir");
        let root = dir.path().to_path_buf();
        // --root names the container directly: meta at the container's top level.
        write_meta_at(&root, CURRENT_META);
        Self {
            dir,
            root,
            via_discovery: false,
        }
    }

    /// A store whose commands are driven by discovery (current dir = tempdir),
    /// for the checks that must observe `.loti/` vs `.loti.conf` resolution. The
    /// container is the discovered `<tempdir>/.loti`.
    pub fn new_discovered() -> Self {
        let dir = TempDir::new().expect("make tempdir");
        let root = dir.path().to_path_buf();
        // Discovery finds `<tempdir>/.loti` as the container: meta at
        // `<tempdir>/.loti/meta`.
        write_meta_at(&root.join(".loti"), CURRENT_META);
        Self {
            dir,
            root,
            via_discovery: true,
        }
    }

    /// The directory `--root` points at / discovery starts from. In `--root`
    /// mode this is the container itself; in discovery mode it is the project
    /// dir whose `.loti` is the container.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A path inside the container. In `--root` mode the container is the root;
    /// in discovery mode it is the root's `.loti`.
    pub fn store_path(&self, rel: &str) -> PathBuf {
        if self.via_discovery {
            self.root.join(".loti").join(rel)
        } else {
            self.root.join(rel)
        }
    }

    /// A path inside the directory `--root`/discovery is anchored at. Equal to
    /// [`Store::store_path`] in `--root` mode; the project dir (not the
    /// container) in discovery mode.
    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Overwrite the store metadata (for version-gate checks that fabricate a
    /// mismatched `format-version`). Targets the container per the drive mode.
    pub fn set_meta(&self, contents: &str) {
        if self.via_discovery {
            write_meta_at(&self.root.join(".loti"), contents);
        } else {
            write_meta_at(&self.root, contents);
        }
    }

    /// A fresh binary invocation against this store, with the given arguments.
    /// The `--root` flag (or current dir) is applied automatically.
    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("loti").expect("locate the loti binary");
        if self.via_discovery {
            cmd.current_dir(&self.root);
        } else {
            cmd.arg("--root").arg(&self.root);
        }
        cmd.args(args);
        // A predictable, non-interactive environment: no colour, no locale
        // surprises. Colour suppression is belt-and-braces; the binary already
        // strips colour when stdout is not a TTY (which it is not here).
        cmd.env("NO_COLOR", "1");
        cmd
    }

    /// Run a command expected to succeed, returning its stdout as a string.
    pub fn ok(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().expect("spawn loti");
        assert!(
            out.status.success(),
            "expected success for {args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run a command feeding `stdin`, expected to succeed; returns stdout.
    pub fn ok_stdin(&self, args: &[&str], stdin: &str) -> String {
        let out = self
            .cmd(args)
            .write_stdin(stdin.to_owned())
            .output()
            .expect("spawn loti");
        assert!(
            out.status.success(),
            "expected success for {args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Run a command expected to fail (non-zero exit); returns its stderr.
    pub fn fail(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().expect("spawn loti");
        assert!(
            !out.status.success(),
            "expected failure for {args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    /// Run a command feeding `stdin`, expected to fail; returns stderr.
    pub fn fail_stdin(&self, args: &[&str], stdin: &str) -> String {
        let out = self
            .cmd(args)
            .write_stdin(stdin.to_owned())
            .output()
            .expect("spawn loti");
        assert!(
            !out.status.success(),
            "expected failure for {args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    // -- convenience seeders ------------------------------------------------

    /// Create an epic with an empty body.
    pub fn epic(&self, id: &str) {
        self.ok_stdin(
            &["epic", "create", id, "--name", id, "--summary", "scope"],
            "",
        );
    }

    /// Create a top-level ticket in an epic and return its `<epic>/<n>` ref by
    /// reading it back off the success line.
    pub fn ticket(&self, epic: &str, name: &str) -> String {
        let out = self.ok_stdin(
            &["ticket", "create", epic, "--name", name, "--summary", "s"],
            "",
        );
        parse_created_ref(&out)
    }

    /// Create a subticket under `parent` and return its `<epic>/<n>` ref.
    pub fn subticket(&self, epic: &str, parent: &str, name: &str) -> String {
        let out = self.ok_stdin(
            &[
                "ticket",
                "create",
                epic,
                "--parent",
                parent,
                "--name",
                name,
                "--summary",
                "s",
            ],
            "",
        );
        parse_created_ref(&out)
    }
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

/// Write `meta` at the top level of the container `dir`, creating it. The
/// container is the only directory loti owns; `meta` lives at `<container>/meta`.
fn write_meta_at(dir: &Path, contents: &str) {
    std::fs::create_dir_all(dir).expect("create container");
    std::fs::write(dir.join("meta"), contents).expect("write meta");
}

/// Extract the `<epic>/<n>` reference from a `loti: created ticket <ref>` line.
fn parse_created_ref(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|l| l.rsplit(' ').next().filter(|r| r.contains('/')))
        .unwrap_or_else(|| panic!("no created ref in output: {stdout}"))
        .to_string()
}

/// Whether any byte of `s` is the ANSI escape (`\x1b`), used to assert machine
/// formats are never coloured.
pub fn contains_ansi(s: &str) -> bool {
    s.as_bytes().contains(&0x1b)
}

/// Raw run of the binary with no store context (for `skill` / `--help-full`).
pub fn run_bare(args: &[&str]) -> Output {
    Command::cargo_bin("loti")
        .expect("locate the loti binary")
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn loti")
}
