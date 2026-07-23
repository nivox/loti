//! Black-box test harness: drives the compiled `loti` binary as a subprocess
//! against a throwaway store, so these checks exercise the shipping CLI exactly
//! as a user (or agent) would, never the crate internals.
//!
//! Every test gets its own temporary store. Two isolation strategies are used:
//!   * `Store::new()` — initialises a store *in place* in a fresh tempdir and
//!     then drives every later command with `--root <tempdir>`. This is the
//!     hermetic default; discovery and the current directory are never touched.
//!   * `Store::new_discovered()` — for the few checks that must observe root
//!     discovery / `.loti.conf`, which run the binary with its current dir set
//!     to a tempdir instead of passing `--root`.
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
const CURRENT_META: &str = "format-version = \"1.1\"\n";

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
    /// A store initialised in place in a fresh tempdir, driven via `--root`.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("make tempdir");
        let root = dir.path().to_path_buf();
        write_meta(&root, CURRENT_META);
        Self {
            dir,
            root,
            via_discovery: false,
        }
    }

    /// A store whose commands are driven by discovery (current dir = root),
    /// for the checks that must observe `.loti/` vs `.loti.conf` resolution.
    pub fn new_discovered() -> Self {
        let dir = TempDir::new().expect("make tempdir");
        let root = dir.path().to_path_buf();
        write_meta(&root, CURRENT_META);
        Self {
            dir,
            root,
            via_discovery: true,
        }
    }

    /// The data-root directory backing this store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A path inside the store root.
    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Overwrite the store metadata (for version-gate checks that fabricate a
    /// mismatched `format-version`).
    pub fn set_meta(&self, contents: &str) {
        write_meta(&self.root, contents);
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

/// Write `.loti/meta` under `root`, creating the marker directory.
fn write_meta(root: &Path, contents: &str) {
    let marker = root.join(".loti");
    std::fs::create_dir_all(&marker).expect("create .loti");
    std::fs::write(marker.join("meta"), contents).expect("write meta");
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
