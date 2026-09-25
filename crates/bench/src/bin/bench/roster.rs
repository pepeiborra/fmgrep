/// How to execute one roster row.
#[derive(Debug)]
pub enum Runner {
    /// Out-of-process fmgrep: an `index` spawn, then one server.
    Child {
        bin_name: &'static str,
        bin_env_var: &'static str,
        /// The row's table name.
        label: &'static str,
        /// `index --store-docs`.
        store_docs: bool,
    },
    /// Measured `rg` run over the shared staged tree.
    Rg,
}

/// One row of the comparison table.
#[derive(Debug)]
pub struct RosterEntry {
    pub runner: Runner,
    /// Selected when `--rows` is absent.
    pub default: bool,
}

/// The bench roster: every row runs every term. Order is load-bearing for
/// `--verify` diff output (`reports[0]` is the baseline): the reference row
/// first, then fmgrep's two index layouts.
pub static ROSTER: &[RosterEntry] = &[
    RosterEntry {
        runner: Runner::Rg,
        default: true,
    },
    RosterEntry {
        runner: Runner::Child {
            bin_name: "fmgrep",
            bin_env_var: "FMGREP_BENCH_BIN",
            label: "fmgrep",
            store_docs: false,
        },
        default: true,
    },
    RosterEntry {
        runner: Runner::Child {
            bin_name: "fmgrep",
            bin_env_var: "FMGREP_BENCH_BIN",
            label: "fmgrep-store-docs",
            store_docs: true,
        },
        default: false,
    },
];

/// Table/`--rows` name of a row.
pub fn row_name(entry: &RosterEntry) -> &'static str {
    match &entry.runner {
        Runner::Child { label, .. } => label,
        Runner::Rg => "rg",
    }
}

/// Rows selected by `--rows a,b`: roster order, duplicates collapsed.
/// `None` is the default rows; an unknown name is a loud error listing the
/// roster — a typo must never silently shrink the comparison.
pub fn select_rows(spec: Option<&str>) -> Result<Vec<&'static RosterEntry>, String> {
    let Some(spec) = spec else {
        return Ok(ROSTER.iter().filter(|e| e.default).collect());
    };
    let wanted: Vec<&str> = spec
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if wanted.is_empty() {
        return Err("--rows requires at least one row name".to_string());
    }
    let known: Vec<&str> = ROSTER.iter().map(row_name).collect();
    for name in &wanted {
        if !known.contains(name) {
            return Err(format!(
                "--rows {name:?} is not a bench row: expected one of {}",
                known.join(", ")
            ));
        }
    }
    Ok(ROSTER
        .iter()
        .filter(|entry| wanted.contains(&row_name(entry)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry_name(entry: &RosterEntry) -> &'static str {
        row_name(entry)
    }

    #[test]
    fn rows_filter_selects_named_rows() {
        let all = select_rows(Some("rg,fmgrep,fmgrep-store-docs")).unwrap();
        assert_eq!(all.len(), ROSTER.len());
        // Roster order, not the order given, and duplicates collapse.
        let some = select_rows(Some("fmgrep-store-docs,rg,rg")).unwrap();
        assert_eq!(
            some.iter().map(|e| row_name(e)).collect::<Vec<_>>(),
            ["rg", "fmgrep-store-docs"]
        );
    }

    #[test]
    fn rows_filter_rejects_unknown_name() {
        for bogus in ["nope", "rg,nope", ""] {
            let err = select_rows(Some(bogus)).unwrap_err();
            assert!(err.contains("--rows"), "bogus {bogus:?}: {err}");
        }
        let err = select_rows(Some("nope")).unwrap_err();
        for name in ROSTER.iter().map(row_name) {
            assert!(err.contains(name), "{err}");
        }
    }

    #[test]
    fn default_rows_are_rg_then_fmgrep() {
        let rows = select_rows(None).unwrap();
        assert_eq!(
            rows.iter().map(|e| row_name(e)).collect::<Vec<_>>(),
            ["rg", "fmgrep"]
        );
    }

    #[test]
    fn roster_has_three_rows_in_order() {
        assert_eq!(
            ROSTER.iter().map(entry_name).collect::<Vec<_>>(),
            ["rg", "fmgrep", "fmgrep-store-docs"]
        );
    }
}
