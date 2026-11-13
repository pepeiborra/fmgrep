//! Real git repositories in a temporary directory, with an environment of
//! their own: `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` point at empty
//! files, and `HOME` and `XDG_CONFIG_HOME` at directories of the sandbox,
//! so that neither the user's git config nor their global excludes file
//! reach git or fmgrep. Dates are fixed, so commits are the same on every
//! run.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct Sandbox {
    pub tmp: tempfile::TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("fmgrep-git-")
            .tempdir()
            .unwrap();
        for dir in ["home", "config", "cache"] {
            std::fs::create_dir(tmp.path().join(dir)).unwrap();
        }
        std::fs::write(tmp.path().join("gitconfig"), "").unwrap();
        std::fs::write(tmp.path().join("gitconfig-system"), "").unwrap();
        Self { tmp }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.tmp.path().join(rel)
    }

    /// The global git config file, empty until a test writes it.
    pub fn global_config(&self) -> PathBuf {
        self.path("gitconfig")
    }

    /// This sandbox's environment on `cmd`.
    pub fn env<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "FMGREP_SERVER",
            "FMGREP_VERBOSE",
        ] {
            cmd.env_remove(var);
        }
        let date = "2026-01-01T00:00:00Z";
        cmd.env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("GIT_CONFIG_GLOBAL", self.global_config())
            .env("GIT_CONFIG_SYSTEM", self.path("gitconfig-system"))
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_DATE", date)
            .env("WATCHMAN_SOCK", "/nonexistent")
    }

    /// `git <args>` in `dir`, which must succeed: its stdout.
    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.git_output(dir, args);
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    pub fn git_output(&self, dir: &Path, args: &[&str]) -> Output {
        self.env(Command::new("git").current_dir(dir).args(args))
            .output()
            .expect("spawn git")
    }

    /// A new repository at `rel` holding `files`, committed.
    pub fn repo(&self, rel: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = self.path(rel);
        std::fs::create_dir_all(&dir).unwrap();
        self.git(&dir, &["init", "-q", "-b", "main"]);
        self.write(&dir, files);
        self.commit(&dir, "first");
        std::fs::canonicalize(dir).unwrap()
    }

    pub fn write(&self, dir: &Path, files: &[(&str, &str)]) {
        for (rel, text) in files {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    /// Every change in `dir`, committed.
    pub fn commit(&self, dir: &Path, message: &str) -> String {
        self.git(dir, &["add", "-A"]);
        self.git(dir, &["commit", "-q", "--allow-empty", "-m", message]);
        self.head(dir)
    }

    pub fn head(&self, dir: &Path) -> String {
        self.git(dir, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// fmgrep in `dir`, in this sandbox's environment.
    pub fn fmgrep(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fmgrep"));
        cmd.current_dir(dir);
        self.env(&mut cmd);
        cmd
    }

    pub fn run(&self, dir: &Path, args: &[&str]) -> Output {
        self.fmgrep(dir).args(args).output().expect("spawn fmgrep")
    }

    /// `index --store-docs` of `dir` into the sandbox's `name`: the index
    /// and the build's stderr.
    pub fn index(&self, dir: &Path, name: &str) -> (PathBuf, String) {
        let index = self.path(name);
        let output = index.display().to_string();
        let out = self.run(dir, &["index", "--store-docs", ".", "--output", &output]);
        assert!(
            out.status.success(),
            "index: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        (index, String::from_utf8(out.stderr).unwrap())
    }
}

impl Sandbox {
    /// `fmgrep serve <index> <flags>` over TCP: the server and its address.
    pub fn serve(&self, index: &Path, flags: &[&str]) -> (crate::Server, String) {
        let mut cmd = self.fmgrep(self.tmp.path());
        cmd.arg("serve").arg(index).args(flags);
        let mut server = crate::Server::spawn_cmd(&mut cmd);
        let addr = server.addr();
        assert!(!addr.is_empty(), "serve {} did not start", index.display());
        (server, addr)
    }

    /// `--status` of the server at `addr`.
    pub fn status(&self, addr: &str) -> serde_json::Value {
        let out = self.run(self.tmp.path(), &["--server", addr, "--status"]);
        assert!(out.status.success());
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

/// The `commit` a manifest records.
pub fn commit_of(index: &Path) -> Option<String> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(index.join("manifest.json")).unwrap()).unwrap();
    manifest["commit"]["id"].as_str().map(str::to_owned)
}
