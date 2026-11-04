//! Scoped, ordered processing with bounded work in flight.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard};

/// A discovered path with its size estimate captured by the parallel walker.
pub(crate) struct Candidate {
    pub(crate) path: PathBuf,
    pub(crate) size: u64,
}

const BYTE_BUDGET: u64 = 32 * 1024 * 1024;

pub(crate) fn thread_count() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(12)
}

struct State<T> {
    next: usize,
    consumed: usize,
    bytes: u64,
    canceled: bool,
    ready: BTreeMap<usize, T>,
}

struct Window<T> {
    state: Mutex<State<T>>,
    changed: Condvar,
    sizes: Vec<u64>,
    capacity: usize,
}

impl<T> Window<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn claim(&self) -> Option<usize> {
        let mut state = self.lock();
        loop {
            if state.canceled || state.next == self.sizes.len() {
                return None;
            }
            let size = self.sizes[state.next];
            let empty = state.next == state.consumed;
            if state.next - state.consumed < self.capacity
                && (empty || state.bytes.saturating_add(size) <= BYTE_BUDGET)
            {
                let index = state.next;
                state.next += 1;
                state.bytes = state.bytes.saturating_add(size);
                return Some(index);
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn deposit(&self, index: usize, result: T) {
        self.lock().ready.insert(index, result);
        self.changed.notify_all();
    }

    fn take(&self) -> Option<T> {
        let mut state = self.lock();
        loop {
            if state.canceled || state.consumed == self.sizes.len() {
                return None;
            }
            let index = state.consumed;
            if let Some(result) = state.ready.remove(&index) {
                return Some(result);
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn advance(&self) {
        let mut state = self.lock();
        state.bytes = state.bytes.saturating_sub(self.sizes[state.consumed]);
        state.consumed += 1;
        self.changed.notify_all();
    }

    fn cancel(&self) {
        self.lock().canceled = true;
        self.changed.notify_all();
    }
}

struct CancelOnPanic<'a, T>(&'a Window<T>);

impl<T> Drop for CancelOnPanic<'_, T> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.cancel();
        }
    }
}

/// Every path, including a skipped or failed path, produces one result.
/// The consumer runs in path order and can stop a read-only run by returning
/// false. The window covers processing, queued results, and consumption.
pub(crate) fn ordered<S, T: Send>(
    paths: &[Candidate],
    initialize: impl Fn() -> S + Sync,
    process: impl Fn(&mut S, &Path) -> T + Sync,
    mut consume: impl FnMut(T) -> bool,
) {
    if paths.is_empty() {
        return;
    }
    let threads = thread_count().min(paths.len());
    let window = Window {
        state: Mutex::new(State {
            next: 0,
            consumed: 0,
            bytes: 0,
            canceled: false,
            ready: BTreeMap::new(),
        }),
        changed: Condvar::new(),
        sizes: paths
            .iter()
            .map(|candidate| candidate.size.max(1))
            .collect(),
        capacity: threads * 2,
    };
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let (window, initialize, process) = (&window, &initialize, &process);
            scope.spawn(move || {
                let _guard = CancelOnPanic(window);
                let mut worker = initialize();
                while let Some(index) = window.claim() {
                    window.deposit(index, process(&mut worker, &paths[index].path));
                }
            });
        }
        let _guard = CancelOnPanic(&window);
        while let Some(result) = window.take() {
            if !consume(result) {
                window.cancel();
                break;
            }
            window.advance();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(sizes: Vec<u64>, capacity: usize) -> Window<usize> {
        Window {
            state: Mutex::new(State {
                next: 0,
                consumed: 0,
                bytes: 0,
                canceled: false,
                ready: BTreeMap::new(),
            }),
            changed: Condvar::new(),
            sizes,
            capacity,
        }
    }

    #[test]
    fn oversized_file_runs_alone_and_releases_budget() {
        let window = window(vec![BYTE_BUDGET + 1, 1], 2);
        assert_eq!(window.claim(), Some(0));
        assert_eq!(window.lock().bytes, BYTE_BUDGET + 1);
        window.deposit(0, 42);
        assert_eq!(window.take(), Some(42));
        window.advance();
        assert_eq!(window.claim(), Some(1));
    }

    #[test]
    fn captured_sizes_keep_oversized_candidates_sequential_without_restat() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let paths: Vec<_> = (0..16)
            .map(|index| Candidate {
                path: PathBuf::from(format!("absent-candidate-{index}")),
                size: BYTE_BUDGET + 1,
            })
            .collect();
        let outstanding = AtomicUsize::new(0);
        let mut consumed = 0;
        ordered(
            &paths,
            || (),
            |(), _| outstanding.fetch_add(1, Ordering::SeqCst) + 1,
            |in_flight| {
                assert_eq!(in_flight, 1);
                assert_eq!(outstanding.fetch_sub(1, Ordering::SeqCst), 1);
                consumed += 1;
                true
            },
        );
        assert_eq!(consumed, paths.len());
    }

    #[test]
    fn consumption_is_ordered_even_when_workers_finish_out_of_order() {
        let window = window(vec![1, 1], 2);
        assert_eq!(window.claim(), Some(0));
        assert_eq!(window.claim(), Some(1));
        window.deposit(1, 20);
        window.deposit(0, 10);
        assert_eq!(window.take(), Some(10));
        window.advance();
        assert_eq!(window.take(), Some(20));
        window.advance();
        assert_eq!(window.take(), None);
    }

    #[test]
    fn cancellation_stops_claims_and_consumption() {
        let window = window(vec![1], 1);
        window.cancel();
        assert_eq!(window.claim(), None);
        assert_eq!(window.take(), None);
    }
}
