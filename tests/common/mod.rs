//! The process's workers as a host installs them, here scoped threads as
//! many as [`workers`] last asked for, so a test runs the codec on one
//! thread or several and compares.
use kinogaki_studio_api::geometry::parallel::{self, Workers};
use std::ops::Range;
use std::panic::Location;
use std::sync::atomic::{AtomicUsize, Ordering};

static THREADS: AtomicUsize = AtomicUsize::new(1);

struct Scoped;

impl Workers for Scoped {
    fn run(
        &self,
        _: &'static Location<'static>,
        len: usize,
        minimum: usize,
        work: &(dyn Fn(Range<usize>) -> bool + Sync),
    ) {
        let threads = THREADS.load(Ordering::Relaxed).max(1);
        let block = minimum.max(len.div_ceil(threads)).max(1);
        std::thread::scope(|scope| {
            for start in (0..len).step_by(block) {
                scope.spawn(move || work(start..(start + block).min(len)));
            }
        });
    }
}

/// From now on the codec runs on `threads` threads (1: the calling
/// thread's work in blocks, as with a host's workers all busy).
pub fn workers(threads: usize) {
    parallel::install(&Scoped);
    THREADS.store(threads, Ordering::Relaxed);
}
