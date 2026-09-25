//! `serve` + `--server` through `fmgrep`: the shared scenarios.

use crate::Fmgrep;

const BOTH: Fmgrep = Fmgrep {
    store_docs: &[false, true],
};

crate::tests!(BOTH;
    serve::tcp_server_flag_equals_local_search,
    serve::serve_path_finds_the_dotdir,
    serve::stdio_round_trip,
    serve::expr_round_trip,
    serve::flags_round_trip,
);

const FILES: Fmgrep = Fmgrep {
    store_docs: &[false],
};

mod files {
    use super::*;

    crate::tests!(FILES;
        serve::serve_watch_scans_a_changed_file,
        serve::serve_watch_excludes_a_deleted_file,
    );
}
