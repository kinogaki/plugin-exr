//! Independent tasks on the process's workers, the SDK's (on the calling
//! thread alone where no host installed any): chunks decompress and
//! compress within the host's one budget, never on threads of their own.
use kinogaki_studio_api::geometry::parallel;
use std::convert::Infallible;

/// `f(i)` for `i` in `0..count`, results in order; the first error by index
/// wins.
#[track_caller]
pub(crate) fn map<T: Send, E: Send>(
    count: usize,
    f: impl Fn(usize) -> Result<T, E> + Sync,
) -> Result<Vec<T>, E> {
    match parallel::map_blocks(count, 1, |range| Ok::<_, Infallible>(f(range.start))) {
        Ok(results) => results.into_iter().collect(),
        Err(never) => match never {},
    }
}
