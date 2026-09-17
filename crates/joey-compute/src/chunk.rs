//! Cooperative chunked execution (feature 033, US5; contracts/api.md §2).
//!
//! `run_chunked` processes items in fixed-size chunks, checking the cancel
//! token BETWEEN chunks only (never mid-chunk — chunk bodies stay simple
//! and lock-free). Returns the partial results accumulated so far plus a
//! completed-all flag (`false` iff cancellation was observed).

use crate::CancelToken;

/// Run `f` over `items` in `chunk_size`-sized chunks, checking
/// `cancel_token` between chunks. Returns `(partial_results,
/// completed_all)`.
///
/// - Cancellation observed before a chunk ⇒ that chunk and everything
///   after it is skipped; `completed_all` is `false`.
/// - A pre-cancelled token processes nothing.
/// - `chunk_size` of 0 is treated as 1 (a chunk must contain at least one
///   item to make progress).
pub fn run_chunked<T, R, F>(
    items: &[T],
    cancel_token: &CancelToken,
    chunk_size: usize,
    mut f: F,
) -> (Vec<R>, bool)
where
    F: FnMut(&[T]) -> Vec<R>,
{
    let chunk_size = chunk_size.max(1);
    let mut out: Vec<R> = Vec::with_capacity(items.len());
    for chunk in items.chunks(chunk_size) {
        if cancel_token.is_cancelled() {
            return (out, false);
        }
        out.extend(f(chunk));
    }
    (out, true)
}
