//! Scoped-thread fan-out for construction.

/// Positive chunk length cutting `len` into at most `threads` chunks.
pub fn chunk_len(len: usize, threads: usize) -> usize {
    len.div_ceil(threads.max(1)).max(1)
}

/// `f` over `items`, one scoped thread each (inline for one item); results
/// in item order.
pub fn map<I: Send, R: Send>(items: Vec<I>, f: impl Fn(I) -> R + Sync) -> Vec<R> {
    if items.len() <= 1 {
        return items.into_iter().map(f).collect();
    }
    let f = &f;
    std::thread::scope(|scope| {
        let workers: Vec<_> = items
            .into_iter()
            .map(|item| scope.spawn(move || f(item)))
            .collect();
        workers
            .into_iter()
            .map(|w| {
                w.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}
