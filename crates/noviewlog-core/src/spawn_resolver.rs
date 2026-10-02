//! Background spawn resolution with per-launch caching (issue #59).
//!
//! `prepare_spawn` on Windows probes the cwd dir, every PATH dir merged with
//! the HKLM/HKCU registry PATH, plus well-known node dirs — one `is_file()` /
//! `metadata()` per candidate. A PATH entry on a dead network share or behind
//! aggressive AV hitches the Slint tick on every Start/Restart.
//!
//! [`SpawnResolver`] moves the probing onto worker threads and caches the
//! prepared plan per `(command, args, cwd, PATH snapshot)`, so Start/Restart
//! resolves with zero filesystem/registry probing once warm. The PATH
//! snapshot is part of the cache key, so a changed PATH re-resolves.
//!
//! Failure semantics: a failed resolution is cached so the pending spawn can
//! surface the error exactly once, but every new Start retries fresh
//! ([`SpawnResolver::request_fresh`]) — installing the tool into a directory
//! that is already on PATH must succeed without an app restart. The merged
//! PATH itself (process env + registry) is cached for the process lifetime,
//! so PATH entries an installer adds *after* launch are picked up on the
//! next app start, not by a fresh retry.

use crate::spawn_resolve::{prepare_spawn, PreparedSpawn};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard};

/// Everything `prepare_spawn` can observe — two requests with an equal key
/// always produce the same [`PreparedSpawn`].
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SpawnResolveKey {
    command: String,
    args: Vec<String>,
    cwd: String,
    /// Snapshot of the env inputs to executable resolution (PATH, PATHEXT).
    /// Only a cache-key discriminator; never parsed back. The registry PATH
    /// merge is process-lifetime cached and cannot change mid-process.
    path_snapshot: u64,
}

impl SpawnResolveKey {
    pub fn new(command: &str, args: &[String], cwd: &str) -> Self {
        Self::with_snapshot(command, args, cwd, current_path_snapshot())
    }

    pub fn with_snapshot(command: &str, args: &[String], cwd: &str, path_snapshot: u64) -> Self {
        Self {
            command: command.to_string(),
            args: args.to_vec(),
            cwd: cwd.to_string(),
            path_snapshot,
        }
    }
}

/// Hash of the env vars that influence executable resolution. When they
/// change, cache keys change and the next request re-resolves (acceptance:
/// "PATH changes still picked up").
pub fn current_path_snapshot() -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::env::var_os("PATH").hash(&mut hasher);
    std::env::var_os("PATHEXT").hash(&mut hasher);
    hasher.finish()
}

pub(crate) type ResolveResult = Result<PreparedSpawn, String>;
type ResolveFn = Arc<dyn Fn(&str, Vec<String>, &str) -> ResolveResult + Send + Sync>;

#[derive(Default)]
struct Slot {
    ready: Option<ResolveResult>,
    inflight: bool,
}

/// Cloneable handle to the shared resolution cache. Cheap to hold on the
/// engine and on every worker thread.
#[derive(Clone)]
pub struct SpawnResolver {
    slots: Arc<Mutex<HashMap<SpawnResolveKey, Slot>>>,
    resolve: ResolveFn,
}

impl Default for SpawnResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl SpawnResolver {
    /// Real resolver backed by [`prepare_spawn`].
    pub fn new() -> Self {
        Self::with_resolve_fn(Arc::new(|command, args, cwd| {
            prepare_spawn(command, args, Some(cwd))
        }))
    }

    /// Injectable resolve function for tests.
    pub fn with_resolve_fn(resolve: ResolveFn) -> Self {
        Self {
            slots: Arc::new(Mutex::new(HashMap::new())),
            resolve,
        }
    }

    fn lock_slots(&self) -> MutexGuard<'_, HashMap<SpawnResolveKey, Slot>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ask for a resolution of `(command, args, cwd)`.
    ///
    /// Returns the cached result immediately when warm (success **or** a
    /// previous failure — a parked spawn must learn about the failure so it
    /// can surface it); kicks a worker thread on a cold miss and returns
    /// `None` until it lands. Concurrent requests for the same key share one
    /// worker. Used by the engine's pending-spawn drain.
    pub fn request(&self, command: &str, args: Vec<String>, cwd: &str) -> Option<ResolveResult> {
        self.request_inner(command, args, cwd, false)
    }

    /// Like [`Self::request`], but a cached **failure** is treated as stale:
    /// a fresh worker is kicked, so installing the tool into a directory
    /// that is already on PATH makes the next Start succeed without an app
    /// restart. (PATH entries added after launch need the next app start —
    /// the merged PATH is process-lifetime cached.) Used when a Start or a
    /// prewarm asks for a launch the user is actively trying to run.
    pub fn request_fresh(
        &self,
        command: &str,
        args: Vec<String>,
        cwd: &str,
    ) -> Option<ResolveResult> {
        self.request_inner(command, args, cwd, true)
    }

    fn request_inner(
        &self,
        command: &str,
        args: Vec<String>,
        cwd: &str,
        retry_failed: bool,
    ) -> Option<ResolveResult> {
        let key = SpawnResolveKey::new(command, &args, cwd);
        // Decide atomically: return a ready result, wait on an inflight
        // worker, or claim this key for a new worker.
        let (result, kick) = {
            let mut slots = self.lock_slots();
            let slot = slots.entry(key.clone()).or_default();
            if let Some(ready) = slot.ready.as_ref() {
                if retry_failed && ready.is_err() {
                    if slot.inflight {
                        (None, false)
                    } else {
                        slot.inflight = true;
                        (None, true)
                    }
                } else if ready.is_err() && slot.inflight {
                    // A fresh retry for this key is running (a Start asked for
                    // one) — the drain must wait for it instead of surfacing
                    // the stale error.
                    (None, false)
                } else {
                    (Some(ready.clone()), false)
                }
            } else if slot.inflight {
                (None, false)
            } else {
                slot.inflight = true;
                (None, true)
            }
        };
        if !kick {
            return result;
        }

        let resolve = self.resolve.clone();
        let slots = self.slots.clone();
        let owned_command = command.to_string();
        let owned_cwd = cwd.to_string();
        std::thread::spawn(move || {
            // A panicking resolve must not poison the slot into a permanent
            // "inflight" state — treat a panic like a failure.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                resolve(&owned_command, args, &owned_cwd)
            }))
            .unwrap_or_else(|_| Err("spawn resolver panicked".to_string()));
            let mut slots = slots.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(slot) = slots.get_mut(&key) {
                // Successes and failures are both stored (failures so a parked
                // spawn surfaces them); `request_fresh` retries stale failures.
                slot.ready = Some(result);
                slot.inflight = false;
            }
        });
        None
    }

    /// Test hook: whether a ready result is cached for this key.
    #[cfg(test)]
    pub fn is_cached(&self, command: &str, args: &[String], cwd: &str) -> bool {
        let key = SpawnResolveKey::new(command, args, cwd);
        self.lock_slots()
            .get(&key)
            .and_then(|s| s.ready.as_ref())
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn counting_resolver(delay: Duration, fail: bool) -> (SpawnResolver, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let resolve: ResolveFn = Arc::new(move |command, args, cwd| {
            calls2.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(delay);
            if fail {
                return Err(format!("not found: {command}"));
            }
            Ok(PreparedSpawn {
                command: command.to_string(),
                args,
                cwd: cwd.to_string(),
            })
        });
        (SpawnResolver::with_resolve_fn(resolve), calls)
    }

    /// Poll until `request` returns a ready result (worker threads are async).
    fn wait_ready(resolver: &SpawnResolver, command: &str, args: &[String], cwd: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if resolver.request(command, args.to_vec(), cwd).is_some() {
                return;
            }
            assert!(Instant::now() < deadline, "resolution never completed");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn request_resolves_in_background_then_caches() {
        let (resolver, calls) = counting_resolver(Duration::from_millis(30), false);
        assert!(
            resolver
                .request("node", vec!["a".into()], r"C:\proj")
                .is_none(),
            "cold cache must defer to the worker thread"
        );
        wait_ready(&resolver, "node", &["a".into()], r"C:\proj");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Warm: served from cache, no new resolution.
        let again = resolver
            .request("node", vec!["a".into()], r"C:\proj")
            .expect("cached")
            .expect("cached success");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(again.command, "node");
        assert_eq!(again.cwd, r"C:\proj");
        assert!(resolver.is_cached("node", &["a".into()], r"C:\proj"));
    }

    #[test]
    fn inflight_requests_share_one_worker() {
        let (resolver, calls) = counting_resolver(Duration::from_millis(80), false);
        for _ in 0..8 {
            assert!(resolver.request("node", vec![], r"C:\proj").is_none());
        }
        wait_ready(&resolver, "node", &[], r"C:\proj");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one worker per key");
    }

    #[test]
    fn different_args_are_separate_cache_entries() {
        let (resolver, calls) = counting_resolver(Duration::from_millis(20), false);
        assert!(resolver
            .request("node", vec!["a".into()], r"C:\p")
            .is_none());
        assert!(resolver
            .request("node", vec!["b".into()], r"C:\p")
            .is_none());
        wait_ready(&resolver, "node", &["a".into()], r"C:\p");
        wait_ready(&resolver, "node", &["b".into()], r"C:\p");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn path_snapshot_changes_the_cache_key() {
        let a = SpawnResolveKey::with_snapshot("node", &[], r"C:\p", 1);
        let b = SpawnResolveKey::with_snapshot("node", &[], r"C:\p", 2);
        assert_ne!(a, b);
        assert_eq!(a, SpawnResolveKey::with_snapshot("node", &[], r"C:\p", 1));
        // Snapshot varies with PATH content.
        let _ = current_path_snapshot();
    }

    #[test]
    fn failures_surface_once_then_retry_on_fresh_requests() {
        let (resolver, calls) = counting_resolver(Duration::from_millis(10), true);
        // Start path (fresh): kicks a worker, parks the spawn.
        assert!(resolver.request_fresh("node", vec![], r"C:\p").is_none());
        // Drain path: once the worker failed, the cached error is returned so
        // the pending spawn can surface it and unstick the terminal.
        let deadline = Instant::now() + Duration::from_secs(5);
        let surfaced = loop {
            match resolver.request("node", vec![], r"C:\p") {
                Some(res) => break res,
                None => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        };
        assert!(surfaced.is_err(), "failure must surface to the drain");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // The next Start retries fresh instead of failing forever.
        assert!(resolver.request_fresh("node", vec![], r"C:\p").is_none());
        let deadline = Instant::now() + Duration::from_secs(5);
        while calls.load(Ordering::SeqCst) < 2 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn success_wins_over_stale_failure() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let fail_first = Arc::new(AtomicUsize::new(1));
        let fail_first2 = fail_first.clone();
        let resolve: ResolveFn = Arc::new(move |command, args, cwd| {
            calls2.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(10));
            if fail_first2.swap(0, Ordering::SeqCst) == 1 {
                return Err("transient".to_string());
            }
            Ok(PreparedSpawn {
                command: command.to_string(),
                args,
                cwd: cwd.to_string(),
            })
        });
        let resolver = SpawnResolver::with_resolve_fn(resolve);
        // First attempt fails...
        assert!(resolver.request_fresh("node", vec![], r"C:\p").is_none());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !resolver.is_cached("node", &[], r"C:\p") {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        // ...fresh retry succeeds and the success is served from cache.
        assert!(resolver.request_fresh("node", vec![], r"C:\p").is_none());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match resolver.request("node", vec![], r"C:\p") {
                Some(Ok(prepared)) => {
                    assert_eq!(prepared.command, "node");
                    break;
                }
                Some(Err(err)) => panic!("retry should have succeeded: {err}"),
                None => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        let again = resolver
            .request_fresh("node", vec![], r"C:\p")
            .expect("cached success");
        assert!(again.is_ok());
    }
}
