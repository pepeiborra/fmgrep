//! Bench binary: one comparison table over one corpus (a checkout, every
//! corpus merged, or either replicated N times). `bench --help` lists the
//! flags; `/.../` terms are regexes, the rest literals.

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

mod child_spawn;
mod cli;
mod corpus;
mod freshness;
mod load;
mod machine;
mod markdown;
mod mem;
mod preflight;
mod report;
mod rg_mem;
mod roster;
mod rss;
mod run;
mod serve;
mod subprocess;
mod verify;

use bench::terms::{TermSet, term_set};
use bench::{TraceOptions, write_jsonl_trace_with_options};
use cli::{
    DEFAULT_TRACE, corpus_raw_arg, has_flag, parse_arg, print_usage, results_arg, source_line,
    warn_on_unknown_args,
};
use corpus::compose::{LoadedCorpus, load};
use corpus::selector::CorpusSelector;
use report::{fmt_bytes, print_comparison, print_report};
use roster::select_rows;
use rss::rusage_self_max_rss_bytes;
use run::{run_selected, stage_shared};
use verify::{verify_replica_multiples, verify_reports};

fn make_run_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("bench-{ms}-{}", std::process::id())
}

/// Exit 2 with `e` on stderr: every usage error leaves before any work.
fn usage_error(e: impl std::fmt::Display) -> ! {
    eprintln!("error: {e}");
    std::process::exit(2)
}

fn print_corpus_header(loaded: &LoadedCorpus, label: &str) {
    let view = loaded.corpus();
    println!();
    let suffix = if loaded.fixture { " (fixture)" } else { "" };
    println!("== corpus: {label}{suffix} ==");
    if let [only] = &loaded.members[..] {
        println!("corpus root: {}", only.root.display());
    } else {
        let roots: Vec<String> = loaded
            .members
            .iter()
            .map(|m| format!("{}={}", m.id, m.root.display()))
            .collect();
        println!("corpus roots: {}", roots.join(", "));
        let members: Vec<String> = loaded
            .members
            .iter()
            .map(|m| format!("{} {} files {}", m.id, m.num_docs, fmt_bytes(m.num_bytes)))
            .collect();
        println!("corpus members: {}", members.join(", "));
    }
    if loaded.fixture {
        println!(
            "corpus source: FIXTURE — tiny built-in docs, NOT a real checkout; numbers are not corpus numbers"
        );
    } else {
        let plural = if loaded.members.len() > 1 { "s" } else { "" };
        match loaded.revision() {
            Some(sha) => println!("corpus revision{plural}: {sha}"),
            None => println!("corpus revision{plural}: unknown (corpus dir is not a git checkout)"),
        }
    }
    // Corpus size over the same enumeration the engines index
    // (bench::discover_index_docs), so this count equals every row's
    // `files` and a direct `index <root>` run's num_files.
    let replicas = if loaded.replicas > 1 {
        format!(
            " — {} replicas of {} files / {}",
            loaded.replicas,
            loaded.base_view().num_docs(),
            fmt_bytes(loaded.base_view().total_bytes())
        )
    } else {
        String::new()
    };
    println!(
        "corpus size: {} ({} indexable files){replicas}",
        fmt_bytes(view.total_bytes()),
        view.num_docs()
    );
    println!("loaded {} docs (whole corpus)", view.num_docs());
    println!(
        "benchmark: one index over the whole corpus, searched per term \
         (bare = literal, /.../ = regex through the engine's OWN path)"
    );
}

/// Past the usage checks every failure returns instead of `process::exit`,
/// so the staging and the composed root are deleted.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if has_flag(&args, "--help") || has_flag(&args, "-h") {
        print_usage();
        return ExitCode::SUCCESS;
    }
    warn_on_unknown_args(&args);

    // --terms overrides the corpus's own query set.
    let terms_override: Option<Vec<String>> = parse_arg(&args, &["--terms"]).and_then(|raw| {
        let custom: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        if custom.is_empty() {
            eprintln!("warning: --terms given but empty; using the corpus's set");
            None
        } else {
            // --terms values pass through untouched, `/.../` spelling
            // included: a pattern is one the engines compile themselves.
            Some(custom)
        }
    });
    let trace_path = parse_arg(&args, &["--trace"]).unwrap_or_else(|| DEFAULT_TRACE.to_string());
    let verify = has_flag(&args, "--verify");
    let dry_run = has_flag(&args, "--dry-run");
    let allow_stale = has_flag(&args, "--allow-stale");
    // Children read the env knob, so the flag is forwarded by setting it.
    let verbose = has_flag(&args, "--verbose") || has_flag(&args, "-v");
    if verbose {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("FMGREP_VERBOSE", "1") };
    }
    bench::set_verbose(verbose);
    // Selection errors (unknown base, bad replica count, repeated or
    // value-less flag, unknown row) exit 2 here, before any corpus walk,
    // run-id printing or trace truncation.
    let corpus_raw = corpus_raw_arg(&args).unwrap_or_else(|e| usage_error(e));
    if has_flag(&args, "--print-source") {
        println!(
            "{}",
            source_line(corpus_raw.as_deref()).unwrap_or_else(|e| usage_error(e))
        );
        return ExitCode::SUCCESS;
    }
    let results_path = results_arg(&args).unwrap_or_else(|e| usage_error(e));
    let corpus_default = CorpusSelector::is_default(corpus_raw.as_deref());
    let selector =
        CorpusSelector::resolve(corpus_raw.as_deref()).unwrap_or_else(|e| usage_error(e));
    let rows =
        select_rows(parse_arg(&args, &["--rows"]).as_deref()).unwrap_or_else(|e| usage_error(e));
    if verify && rows.len() < 2 {
        usage_error(format!(
            "--verify compares rows against each other, so it needs at least 2; --rows selected {}",
            rows.len()
        ));
    }
    let corpus_id = selector.corpus_id();

    let run_id = make_run_id();
    println!("run_id: {run_id}");
    // Corpus selection is never silent: this line always names the effective
    // selection (marked `(default)` when --corpus was absent).
    println!(
        "corpus selection: {}",
        selector.header_label(corpus_default)
    );
    println!(
        "rows: {}",
        rows.iter()
            .map(|e| roster::row_name(e))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let bins = match freshness::row_binaries(&rows) {
        Ok(bins) => bins,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    freshness::print_header(&bins);
    if let Some(refusal) = freshness::stale_refusal(&bins) {
        if !allow_stale {
            eprintln!("error: {refusal}");
            return ExitCode::from(2);
        }
        eprintln!("warning: running stale engine binaries (--allow-stale)");
    }

    let loaded = match load(selector) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    print_corpus_header(&loaded, &corpus_id);
    let view = loaded.corpus();
    let corpus_bytes = view.total_bytes();

    let sets: Option<Vec<TermSet>> = loaded.members.iter().map(|m| term_set(m.id)).collect();
    let Some(sets) = sets else {
        eprintln!("error: a corpus of {corpus_id} has no query set");
        return ExitCode::FAILURE;
    };
    let set = match &sets[..] {
        [only] => only.clone(),
        many => TermSet::union(many),
    };
    let terms: Vec<String> = match &terms_override {
        Some(t) => t.clone(),
        None => set.queries.clone(),
    };
    let term_refs: Vec<&str> = terms.iter().map(|s| s.as_str()).collect();
    println!(
        "term set: {} ({} literal, {} regex){}",
        set.name,
        set.literals().count(),
        set.regexes().count(),
        if terms_override.is_some() {
            " — overridden by --terms"
        } else {
            ""
        }
    );
    println!("terms: {}", terms.join(", "));

    // Disk forecast before anything is written; a selection that cannot fit
    // is refused rather than discovered half-way through a row.
    let forecast = preflight::estimate(&rows, loaded.base_view().total_bytes(), loaded.replicas);
    forecast.print();
    if let Some(refusal) = forecast.refusal() {
        eprintln!("error: {refusal}");
        return ExitCode::from(2);
    }
    if dry_run {
        println!("dry run: nothing staged, indexed or queried (--dry-run)");
        report_runner_peak_rss();
        return ExitCode::SUCCESS;
    }

    // Truncate the combined trace up front so stale lines from previous runs
    // can't pollute it; the per-row writers below append.
    if let Err(e) = std::fs::write(&trace_path, "") {
        eprintln!("failed to write trace to {trace_path}: {e}");
        return ExitCode::FAILURE;
    }

    // One staging per run, shared by rg and the child rows (see run.rs).
    let staged = match stage_shared(loaded.base_view(), loaded.replicas) {
        Ok(staged) => staged,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let reports = match run_selected(&rows, &staged, &term_refs) {
        Ok(reports) => reports,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    println!();
    for report in &reports {
        print_report(report);
    }
    print_comparison(
        &reports,
        view.num_docs(),
        corpus_bytes,
        &format!("corpus {corpus_id}"),
    );

    // run_selected keeps roster order, so reports and rows line up.
    for (report, entry) in reports.iter().zip(&rows) {
        let engine_binary = bins
            .iter()
            .find(|b| b.row == roster::row_name(entry))
            .map(|b| b.stamp.clone());
        if let Err(e) = write_jsonl_trace_with_options(
            report,
            &trace_path,
            &TraceOptions {
                run_id: Some(run_id.clone()),
                corpus: Some(corpus_id.clone()),
                corpus_revision: loaded.revision(),
                fixture: loaded.fixture,
                num_docs: view.num_docs(),
                append: true,
                num_replicas: loaded.replicas,
                corpus_members: loaded.member_ids(),
                engine_binary,
            },
        ) {
            eprintln!("failed to write trace to {trace_path}: {e}");
            return ExitCode::FAILURE;
        }
    }
    println!("wrote trace to {trace_path} (corpus {corpus_id})");

    let diffs = verify.then(|| {
        let mut diffs = verify_reports(&corpus_id, &reports);
        diffs.extend(verify_replica_multiples(
            &corpus_id,
            loaded.replicas,
            &reports,
        ));
        diffs
    });
    if let Some(path) = &results_path {
        let fmgrep_bin = bins
            .first()
            .map(|b| std::path::PathBuf::from(&b.stamp.path));
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let stamps = machine::gather(fmgrep_bin.as_deref(), &workspace);
        let single = (loaded.members.len() == 1).then(|| loaded.members[0].id);
        let info = markdown::RunInfo {
            corpus: corpus_id.clone(),
            source: single.and_then(bench::terms::source),
            revision: loaded.revision(),
            fixture: loaded.fixture,
            files: view.num_docs(),
            bytes: corpus_bytes,
            literals: terms
                .iter()
                .filter(|t| bench::regex_pattern(t).is_none())
                .count(),
            regexes: terms
                .iter()
                .filter(|t| bench::regex_pattern(t).is_some())
                .count(),
            custom_terms: terms_override.is_some(),
        };
        let outcome = diffs.as_ref().map(|d| {
            if d.is_empty() {
                Ok(terms.len())
            } else {
                Err(&d[..])
            }
        });
        let md = markdown::render(&info, &stamps, &reports, outcome);
        if let Err(e) = std::fs::write(path, md) {
            eprintln!("failed to write results to {path}: {e}");
            return ExitCode::FAILURE;
        }
        println!("wrote results to {path}");
    }

    if let Some(diffs) = diffs {
        if diffs.is_empty() {
            println!(
                "verify: OK — all {} impl(s) agree on {} term(s) over corpus {corpus_id}",
                reports.len(),
                terms.len()
            );
        } else {
            println!("verify: MISMATCH — {} difference(s):", diffs.len());
            println!(
                "  {:<20} {:<14} {:<12} {:>12} {:>12}",
                "term", "field", "impl", "expected", "actual"
            );
            for d in &diffs {
                println!("  - {d}");
            }
            println!("verify: FAILED — see the MISMATCH lines above");
            report_runner_peak_rss();
            return ExitCode::FAILURE;
        }
    }
    report_runner_peak_rss();
    ExitCode::SUCCESS
}

/// Print the harness process's own peak RSS (children excluded): the runner
/// holds descriptors, not corpus bytes, and this line is what proves it.
fn report_runner_peak_rss() {
    match rusage_self_max_rss_bytes() {
        Some(bytes) => println!("runner peak rss: {}", fmt_bytes(bytes)),
        None => println!("runner peak rss: n/a"),
    }
}
