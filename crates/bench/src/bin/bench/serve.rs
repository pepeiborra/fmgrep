//! Serve-mode rows: one long-lived query server per row, one client
//! round-trip per term, RSS sampled on the server.

use std::time::{Duration, Instant};

use bench::QueryMetrics;

use crate::mem::{Sampler, phase_stats, reaped_peak, sampler_interval, split};

pub struct TermCounts {
    pub total_matches: u64,
    pub docs_matched: usize,
    pub counts_exact: bool,
}

/// One long-lived query server for one row; implementors kill and reap it
/// on drop.
pub trait RowServer {
    /// Spawn; returns the server pid.
    fn start(&mut self) -> Result<u32, String>;
    fn wait_ready(&mut self) -> Result<(), String>;
    /// One client round-trip and its spawn-to-reap wall.
    fn query(&mut self, term: &str) -> Result<(TermCounts, Duration), String>;
    /// Reap the server; `Err` unless it ended cleanly.
    fn stop(&mut self) -> Result<(), String>;
}

pub struct Served {
    pub queries: Vec<QueryMetrics>,
    pub ready_time: Duration,
    /// Every server sample, raised to `reaped_peak`.
    pub query_peak_rss_bytes: Option<u64>,
    /// Samples after ready.
    pub query_avg_rss_bytes: Option<u64>,
    /// `getrusage` peak of the server and its clients.
    pub reaped_peak: Option<u64>,
    /// Footprint over the same samples as the RSS pair; peak unanchored.
    pub query_peak_footprint_bytes: Option<u64>,
    pub query_avg_footprint_bytes: Option<u64>,
}

/// Start, wait ready, one query per term in order, stop. On `Err` no
/// further request is made; the server is left to the implementor's drop.
pub fn serve_row(server: &mut dyn RowServer, terms: &[&str]) -> Result<Served, String> {
    let t0 = Instant::now();
    let pid = server.start()?;
    let sampled_from = t0.elapsed();
    let sampler = sampler_interval().map(|tick| Sampler::start(pid, tick));
    let run = drive(server, terms, t0);
    let samples = sampler.map(Sampler::stop).unwrap_or_default();
    let (ready_time, queries) = run?;
    let reaped = reaped_peak();
    let ready_mark = ready_time.saturating_sub(sampled_from);
    let (all, all_fp) = split(&samples);
    let ready_at = samples.partition_point(|s| s.elapsed < ready_mark);
    let (after_ready, after_ready_fp) = split(&samples[ready_at..]);
    let (query_peak_rss_bytes, _) = phase_stats(&all, reaped);
    let (_, query_avg_rss_bytes) = phase_stats(&after_ready, None);
    let (query_peak_footprint_bytes, _) = phase_stats(&all_fp, None);
    let (_, query_avg_footprint_bytes) = phase_stats(&after_ready_fp, None);
    Ok(Served {
        queries,
        ready_time,
        query_peak_rss_bytes,
        query_avg_rss_bytes,
        reaped_peak: reaped,
        query_peak_footprint_bytes,
        query_avg_footprint_bytes,
    })
}

fn drive(
    server: &mut dyn RowServer,
    terms: &[&str],
    t0: Instant,
) -> Result<(Duration, Vec<QueryMetrics>), String> {
    server.wait_ready()?;
    let ready_time = t0.elapsed();
    let mut queries = Vec::with_capacity(terms.len());
    for term in terms {
        let (counts, latency) = server.query(term)?;
        queries.push(QueryMetrics {
            term: (*term).to_owned(),
            latency_us: latency.as_micros() as u64,
            total_matches: counts.total_matches,
            docs_matched: counts.docs_matched,
            counts_exact: counts.counts_exact,
            latency,
        });
    }
    server.stop()?;
    Ok((ready_time, queries))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-process server: this test process stands in for the server pid.
    #[derive(Default)]
    struct Fake {
        calls: Vec<String>,
        fail_on: Option<&'static str>,
        stop_err: bool,
    }

    impl RowServer for Fake {
        fn start(&mut self) -> Result<u32, String> {
            self.calls.push("start".into());
            Ok(std::process::id())
        }
        fn wait_ready(&mut self) -> Result<(), String> {
            self.calls.push("ready".into());
            Ok(())
        }
        fn query(&mut self, term: &str) -> Result<(TermCounts, Duration), String> {
            self.calls.push(term.to_owned());
            if self.fail_on == Some(term) {
                return Err(format!("client failed on {term}"));
            }
            let n = term.len() as u64;
            let counts = TermCounts {
                total_matches: n,
                docs_matched: 1,
                counts_exact: false,
            };
            Ok((counts, Duration::from_micros(1000 + n)))
        }
        fn stop(&mut self) -> Result<(), String> {
            self.calls.push("stop".into());
            if self.stop_err {
                Err("server exited 3".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn serve_row_keeps_term_order_and_client_latency() {
        let mut fake = Fake::default();
        let served = serve_row(&mut fake, &["ab", "/a.c/", "x"]).unwrap();
        assert_eq!(fake.calls, ["start", "ready", "ab", "/a.c/", "x", "stop"]);
        let terms: Vec<&str> = served.queries.iter().map(|q| q.term.as_str()).collect();
        assert_eq!(terms, ["ab", "/a.c/", "x"]);
        assert_eq!(served.queries[1].latency, Duration::from_micros(1005));
        assert_eq!(served.queries[1].latency_us, 1005);
        assert_eq!(served.queries[1].total_matches, 5);
        assert!(!served.queries[1].counts_exact);
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(
                served.query_peak_rss_bytes.is_some(),
                "the server pid is sampled"
            );
        }
    }

    #[test]
    fn serve_row_propagates_stop_error() {
        let mut fake = Fake {
            stop_err: true,
            ..Fake::default()
        };
        let err = serve_row(&mut fake, &["ab"]).err().unwrap();
        assert_eq!(err, "server exited 3");
    }

    #[test]
    fn serve_row_makes_no_request_after_a_query_error() {
        let mut fake = Fake {
            fail_on: Some("b"),
            ..Fake::default()
        };
        let err = serve_row(&mut fake, &["a", "b", "c"]).err().unwrap();
        assert_eq!(err, "client failed on b");
        assert_eq!(fake.calls, ["start", "ready", "a", "b"]);
    }
}
