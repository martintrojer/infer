// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Concurrent summary store with blocking deduplication.
//!
//! Replaces OCaml's per-domain LRU cache + SQLite roundtrip with a single
//! shared `DashMap`. When multiple threads need the same callee summary,
//! the first thread computes it while others block on `OnceLock` — no
//! restarts, no wasted work.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use sil::procname::Procname;

/// Concurrent store for analysis summaries.
///
/// Uses `DashMap<Procname, Arc<OnceLock<S>>>` so that:
/// - First thread to request a procedure claims the slot and computes the summary
/// - Concurrent threads requesting the same procedure block until computation completes
/// - No procedure is analyzed more than once (even within cycle waves)
pub struct SummaryStore<S> {
    store: DashMap<Procname, Arc<OnceLock<S>>>,
    completed: AtomicUsize,
}

impl<S: Send + Sync + 'static> SummaryStore<S> {
    pub fn new() -> Self {
        Self {
            store: DashMap::new(),
            completed: AtomicUsize::new(0),
        }
    }

    /// Store a summary for a procedure. Overwrites any existing summary.
    pub fn insert(&self, proc_name: Procname, summary: S) {
        let cell = Arc::new(OnceLock::new());
        let _ = cell.set(summary);
        self.store.insert(proc_name, cell);
        self.completed.fetch_add(1, Ordering::Relaxed);
    }

    /// Get a clone of a procedure's summary, if it has been computed.
    ///
    /// Returns `None` if the procedure hasn't been registered, or if
    /// computation is still in progress on another thread (non-blocking check).
    pub fn get(&self, proc_name: &Procname) -> Option<S>
    where
        S: Clone,
    {
        self.store
            .get(proc_name)
            .and_then(|cell| cell.get().cloned())
    }

    /// Get or compute a summary, blocking if another thread is already computing it.
    ///
    /// This is the core deduplication primitive:
    /// - If the summary exists, returns a clone immediately
    /// - If another thread is computing it, blocks until done, then returns a clone
    /// - If no one has started, calls `compute` to produce the summary
    ///
    /// Guarantees each procedure is analyzed exactly once, even when multiple
    /// threads discover the same on-demand callee simultaneously.
    pub fn get_or_compute(&self, proc_name: &Procname, compute: impl FnOnce() -> S) -> S
    where
        S: Clone,
    {
        // Get or create the slot. The DashMap shard lock is held only briefly.
        let cell = self
            .store
            .entry(proc_name.clone())
            .or_insert_with(|| Arc::new(OnceLock::new()))
            .clone();

        // OnceLock::get_or_init blocks if another thread is initializing.
        let was_empty = cell.get().is_none();
        let result = cell.get_or_init(compute).clone();
        if was_empty && cell.get().is_some() {
            self.completed.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    /// Get a clone of a summary by procedure display name (exact or suffix match).
    ///
    /// Tries exact match first, then suffix match (e.g. "leaf" matches
    /// "$TOPLEVEL$CLASS$.leaf"). Useful in tests where the exact `Procname`
    /// variant isn't known.
    pub fn get_by_name(&self, name: &str) -> Option<S>
    where
        S: Clone,
    {
        // Exact match
        if let Some(entry) = self
            .store
            .iter()
            .find(|entry| format!("{}", entry.key()) == name)
        {
            return entry.value().get().cloned();
        }
        // Suffix match (e.g. "leaf" matches "$TOPLEVEL$CLASS$.leaf")
        let suffix = format!(".{name}");
        self.store
            .iter()
            .find(|entry| format!("{}", entry.key()).ends_with(&suffix))
            .and_then(|entry| entry.value().get().cloned())
    }

    /// Check if a summary has been computed for a procedure.
    pub fn contains(&self, proc_name: &Procname) -> bool {
        self.store
            .get(proc_name)
            .is_some_and(|cell| cell.get().is_some())
    }

    /// Number of completed summaries. O(1).
    pub fn len(&self) -> usize {
        self.completed.load(Ordering::Relaxed)
    }

    /// Whether the store has no completed summaries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate over all completed summaries.
    pub fn for_each(&self, f: impl Fn(&Procname, &S)) {
        self.store.iter().for_each(|entry| {
            if let Some(summary) = entry.value().get() {
                f(entry.key(), summary);
            }
        });
    }

    /// Collect all completed summaries into a Vec.
    pub fn to_vec(&self) -> Vec<(Procname, S)>
    where
        S: Clone,
    {
        self.store
            .iter()
            .filter_map(|entry| {
                entry
                    .value()
                    .get()
                    .map(|s| (entry.key().clone(), s.clone()))
            })
            .collect()
    }
}

impl<S: Send + Sync + 'static> Default for SummaryStore<S> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sil::procname::Procname;

    #[test]
    fn test_insert_and_get() {
        let store = SummaryStore::new();
        let pname = Procname::c_from_string("foo");
        store.insert(pname.clone(), 42u32);
        assert_eq!(store.get(&pname), Some(42));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn test_missing_key() {
        let store: SummaryStore<u32> = SummaryStore::new();
        let pname = Procname::c_from_string("missing");
        assert_eq!(store.get(&pname), None);
        assert!(!store.contains(&pname));
    }

    #[test]
    fn test_get_or_compute() {
        let store = SummaryStore::new();
        let pname = Procname::c_from_string("foo");

        // First call computes
        let v = store.get_or_compute(&pname, || 42u32);
        assert_eq!(v, 42);

        // Second call returns cached value, doesn't call compute
        let v = store.get_or_compute(&pname, || panic!("should not be called"));
        assert_eq!(v, 42);
    }

    #[test]
    fn test_get_or_compute_concurrent() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let store = Arc::new(SummaryStore::new());
        let pname = Procname::c_from_string("foo");
        let compute_count = Arc::new(AtomicUsize::new(0));

        // Spawn multiple threads all requesting the same procedure
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let pname = pname.clone();
                let count = compute_count.clone();
                std::thread::spawn(move || {
                    store.get_or_compute(&pname, || {
                        count.fetch_add(1, Ordering::Relaxed);
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        42u32
                    })
                })
            })
            .collect();

        let results: Vec<u32> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All threads should get the same result
        assert!(results.iter().all(|&v| v == 42));
        // Compute should have been called exactly once
        assert_eq!(compute_count.load(Ordering::Relaxed), 1);
    }
}
