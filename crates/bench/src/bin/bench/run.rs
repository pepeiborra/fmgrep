//! Row execution: stage the corpus once, then run every selected row over that
//! staging. Replica 0 is copied and further replicas are hard-linked, so N
//! replicas cost one copy; staging happens with no child alive, so no row's
//! index time or RSS includes it (the `staged corpus in` line reports its
//! cost). Index dirs stay per row.

use std::time::Instant;

use bench::{BenchmarkReport, Corpus, StagedDocs};

use crate::load;
use crate::mem::prime_reaped;
use crate::rg_mem::run_rg_unified;
use crate::roster::{RosterEntry, Runner, row_name};
use crate::subprocess::{ChildSpec, STAGED_EXT, run_child};

/// Stage `replicas` replicas of `base` once for every row, printing what it
/// cost.
pub fn stage_shared(base: Corpus<'_>, replicas: usize) -> Result<StagedDocs, String> {
    let start = Instant::now();
    let staged = StagedDocs::stage_replicated(base, STAGED_EXT, replicas)
        .map_err(|e| format!("cannot stage the corpus:\n{e}"))?;
    println!(
        "staged corpus in {:.1} s ({} files, staged once and shared by the rg and child rows)",
        start.elapsed().as_secs_f64(),
        staged.num_docs()
    );
    Ok(staged)
}

/// Run `rows` in roster order over `terms`, mixed literal and `/.../` terms
/// alike, each row stamped with the box load around it and credited only
/// the RSS peaks of its own children. A row failure is returned, not exited
/// on, so the caller's staging is still dropped (deleted).
pub fn run_selected(
    rows: &[&RosterEntry],
    staged: &StagedDocs,
    terms: &[&str],
) -> Result<Vec<BenchmarkReport>, String> {
    rows.iter()
        .map(|entry| {
            prime_reaped();
            load::stamp(row_name(entry), || match &entry.runner {
                Runner::Child {
                    bin_name,
                    bin_env_var,
                    label,
                    store_docs,
                } => run_child(
                    &ChildSpec {
                        bin_name,
                        bin_env_var,
                        label,
                        store_docs: *store_docs,
                    },
                    staged.dir(),
                    terms,
                )
                .map_err(|e| format!("subprocess benchmark failed:\n{e}")),
                Runner::Rg => run_rg_unified(staged, terms),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row shares one staging, replicas included.
    #[test]
    fn rows_stage_the_corpus_once_replicas_included() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.c"), b"printk").unwrap();
        let docs = bench::discover_index_meta(dir.path()).unwrap();
        let corpus = Corpus {
            root: dir.path(),
            docs: &docs,
        };
        let staged = stage_shared(corpus, 1).unwrap();
        assert_eq!(staged.num_docs(), 1);
        assert!(staged.dir().join("00000000.bin").is_file());

        let staged = stage_shared(corpus, 3).unwrap();
        assert_eq!(staged.num_docs(), 3);
        assert!(staged.dir().join("00000002.bin").is_file());
    }
}
