//! Recursive filesystem watcher with 100 ms trailing-edge debouncing.
//!
//! Events from `notify` are coalesced into a set of unique wiki-relative
//! paths; after a quiet period of [`DEBOUNCE_MS`] the batch is delivered as a
//! `Vec<String>` on an unbounded tokio channel. [`Watcher::stop`] drops the
//! underlying watcher and closes the channel sender.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use tokio::sync::mpsc::{self, UnboundedReceiver};

const DEBOUNCE_MS: u64 = 100;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Default)]
struct DebounceState {
    paths: HashSet<String>,
    deadline: Option<Instant>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Convert an absolute event path to a forward-slash wiki-relative path.
fn relative_to_root(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

pub struct Watcher {
    inner: Option<RecommendedWatcher>,
    stop: Arc<AtomicBool>,
    flusher: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Start watching `wiki_root` recursively. Returns the handle plus the
    /// receiving end of the debounced-change channel.
    ///
    /// # Errors
    /// Fails when the notify backend cannot be created or the root cannot be
    /// watched.
    pub fn start(
        wiki_root: impl Into<PathBuf>,
    ) -> notify::Result<(Self, UnboundedReceiver<Vec<String>>)> {
        let root: PathBuf = wiki_root.into();
        let (tx, rx) = mpsc::unbounded_channel::<Vec<String>>();
        let pending = Arc::new(Mutex::new(DebounceState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut watcher: RecommendedWatcher = notify::recommended_watcher({
            let pending = Arc::clone(&pending);
            let root = root.clone();
            move |res: notify::Result<Event>| {
                let Ok(event) = res else { return };
                let mut state = lock(&pending);
                for path in event.paths {
                    let rel = relative_to_root(&root, &path);
                    if crate::fs::paths::is_ignored_path(&rel) {
                        continue;
                    }
                    state.paths.insert(rel);
                }
                if !state.paths.is_empty() {
                    state.deadline = Some(Instant::now() + Duration::from_millis(DEBOUNCE_MS));
                }
            }
        })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;

        let flusher = {
            let pending = Arc::clone(&pending);
            let tx = tx.clone();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || loop {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
                let flush = {
                    let mut state = lock(&pending);
                    match state.deadline {
                        Some(deadline) if Instant::now() >= deadline => {
                            state.deadline = None;
                            Some(std::mem::take(&mut state.paths))
                        }
                        _ => None,
                    }
                };
                if let Some(paths) = flush {
                    let mut rels: Vec<String> = paths.into_iter().collect();
                    rels.sort();
                    if tx.send(rels).is_err() {
                        break;
                    }
                }
            })
        };

        Ok((
            Self {
                inner: Some(watcher),
                stop,
                flusher: Some(flusher),
            },
            rx,
        ))
    }

    /// Stop watching: drop the underlying watcher, end the flusher thread,
    /// and close the event channel sender.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        self.inner.take();
        if let Some(handle) = self.flusher.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.inner.take();
        if let Some(handle) = self.flusher.take() {
            let _ = handle.join();
        }
    }
}
