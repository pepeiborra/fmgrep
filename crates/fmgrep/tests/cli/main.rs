//! End-to-end scenarios driven through the `fmgrep` binary, once per index
//! layout they serve.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

mod occurrences;
mod roundtrip;
mod scenarios;
mod serve;

/// The binary under test, restricted to some index layouts.
pub struct Fmgrep {
    /// Every `--store-docs` setting to run; the first is the default one.
    pub store_docs: &'static [bool],
}

impl Fmgrep {
    fn command(&self) -> Command {
        Command::new(env!("CARGO_BIN_EXE_fmgrep"))
    }

    pub fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Output {
        self.command()
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("spawn fmgrep: {e}"))
    }

    /// [`Fmgrep::run`] from `dir`.
    pub fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.command()
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("spawn fmgrep: {e}"))
    }

    fn store_docs(&self) -> bool {
        self.store_docs[0]
    }
}

/// `index <folder> --output <index> [--store-docs] <extra...>`.
pub fn index_argv(folder: &Path, index: &Path, store_docs: bool, extra: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "index".to_string(),
        folder.display().to_string(),
        "--output".into(),
        index.display().to_string(),
    ];
    if store_docs {
        argv.push("--store-docs".into());
    }
    argv.extend(extra.iter().map(|s| s.to_string()));
    argv
}

/// One `#[test]` per named scenario, each run against `$bin`:
/// `tests!(BIN; serve::stdio_round_trip, ...)`.
#[macro_export]
macro_rules! tests {
    ($bin:expr; $($module:ident::$name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                $crate::scenarios::$module::$name(&$bin)
            }
        )*
    };
}
