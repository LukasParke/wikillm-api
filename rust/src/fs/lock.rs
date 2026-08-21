//! Per-path async locking.
//!
//! [`PathLock`] hands out one `tokio::sync::Mutex` per path (tokio mutexes are
//! FIFO-fair, matching the TS waiter queue). [`PathLock::acquire_many`] sorts
//! and dedupes paths before acquiring, so overlapping multi-acquires can never
//! deadlock.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{OwnedMutexGuard, Mutex};

/// Registry of per-path async mutexes. Cheap to clone into `Arc` and share.
#[derive(Default)]
pub struct PathLock {
    locks: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl PathLock {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_for(&self, path: &str) -> Arc<Mutex<()>> {
        let mut map = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.entry(path.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Acquire the exclusive lock for a single path; dropping the returned
    /// guard releases it (FIFO among waiters).
    pub async fn acquire(&self, path: &str) -> OwnedMutexGuard<()> {
        self.lock_for(path).lock_owned().await
    }

    /// Run `f` while holding the lock for `path`.
    pub async fn run_exclusive<F, T>(&self, path: &str, f: F) -> T
    where
        F: Future<Output = T>,
    {
        let _guard = self.acquire(path).await;
        f.await
    }

    /// Acquire locks for every distinct path in sorted order — deadlock-free
    /// even when concurrent callers list the same paths in different orders.
    /// All locks release when the returned guard drops.
    pub async fn acquire_many(&self, paths: &[&str]) -> MultiPathGuard {
        let mut sorted = paths.to_vec();
        sorted.sort();
        sorted.dedup();
        let mut guards = Vec::with_capacity(sorted.len());
        for path in sorted {
            guards.push(self.lock_for(&path).lock_owned().await);
        }
        MultiPathGuard { _guards: guards }
    }

    /// Run `f` while holding locks for all `paths`.
    pub async fn run_many_exclusive<F, T>(&self, paths: &[&str], f: F) -> T
    where
        F: Future<Output = T>,
    {
        let _guard = self.acquire_many(paths).await;
        f.await
    }
}

impl std::fmt::Debug for PathLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathLock").finish_non_exhaustive()
    }
}

/// Holds every lock acquired by [`PathLock::acquire_many`] until dropped.
pub struct MultiPathGuard {
    _guards: Vec<OwnedMutexGuard<()>>,
}
