//! The encoders' one rayon helper.

/// `f` of every item, one job per item on the current rayon pool; a lone
/// item runs inline. `f` runs no parallel work, so no thread ever waits
/// inside a job.
pub(crate) fn each<I: Send>(items: impl IntoIterator<Item = I>, f: impl Fn(I) + Sync) {
    let mut items = items.into_iter().peekable();
    let Some(first) = items.next() else { return };
    if items.peek().is_none() {
        return f(first);
    }
    let f = &f;
    rayon::in_place_scope(|scope| {
        for item in std::iter::once(first).chain(items) {
            scope.spawn(move |_| f(item));
        }
    });
}
