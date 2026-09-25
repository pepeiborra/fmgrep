//! `--occurrences` through `fmgrep`.

use crate::Fmgrep;

const FILES: Fmgrep = Fmgrep {
    store_docs: &[false],
};

const STORED: Fmgrep = Fmgrep {
    store_docs: &[true],
};

crate::tests!(FILES;
    occurrences::occurrences_equal_default_totals,
    occurrences::serve_and_server_flag_answer_occurrences,
    occurrences::stale_file_is_not_ignored_by_occurrences,
);

mod stored {
    use super::*;

    crate::tests!(STORED;
        occurrences::occurrences_equal_default_totals,
        occurrences::serve_and_server_flag_answer_occurrences,
    );
}
