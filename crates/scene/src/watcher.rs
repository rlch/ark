//! Scene file watcher — T-11.6 (cavekit-scene R14).
//!
//! Spawns a `notify` watcher on the resolved scene path and fires a
//! callback (typically `reload_scene`) whenever an accepted change
//! lands. Editor temp files are filtered out by filename suffix so
//! atomic-rename save strategies (vim `.swp`, emacs `.#`, rsync `.tmp`,
//! `~` tilde-backup, `.bak`) don't trigger spurious reloads.
//!
//! # Pipeline
//!
//! 1. `notify::recommended_watcher` watches the scene file's parent
//!    directory (watching the file itself misses inode-replacement
//!    editors; parent-dir watches catch every flavour of save).
//! 2. Every raw event is classified via [`accepts_path`]: if the
//!    filename ends in a known editor-temp suffix, it's dropped;
//!    otherwise it's accepted.
//! 3. Accepted events flip a shared "dirty" flag. A debounce thread
//!    polls the flag on a ticker (`watch_debounce_ms` per
//!    [`WatcherConfig`]) and, when it sees dirty + enough elapsed
//!    time since the first change in the burst, fires the callback
//!    once per burst.
//!
//! # Lifetime
//!
//! [`SceneWatcher`] owns the `notify::RecommendedWatcher` + the
//! debounce thread's `JoinHandle`. Dropping the watcher stops the
//! debounce thread via a shared `AtomicBool` and releases the watcher
//! handle (which un-registers the inotify/FSEvents callback). No
//! cleanup is required beyond `drop(watcher)`.
//!
//! # Interaction with [`crate::reload::SceneReloader`]
//!
//! The callback passed to [`SceneWatcher::spawn`] typically closes over
//! an `Arc<SceneReloader>` and calls `reloader.reload(any_turn_inflight)`.
//! Re-entry, turn-inflight gating, and in-flight-reaction drain are
//! handled inside `SceneReloader`; the watcher is just a fire-when-dirty
//! trigger. Rapid reload requests layer the `SceneReloader::reload_pending`
//! flag on top of the watcher's debounce so a burst-during-turn still
//! reloads once when the turn clears.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

/// Editor temp-file suffixes the watcher silently drops.
///
/// Covers the flavours of save the T-11.6 spec calls out plus the
/// common `.bak` backup convention:
///
/// | Pattern                | Tool       | Notes |
/// |------------------------|------------|-------|
/// | trailing `~`           | vim, emacs | backup file |
/// | `.swp` / `.swo` suffix | vim        | swap file |
/// | `.tmp` suffix          | rsync, etc | temp-then-rename atomic save |
/// | leading `.#`           | emacs      | lock file |
/// | `.bak` suffix          | generic    | backup file |
const EDITOR_SUFFIX_REJECT: &[&str] = &[".swp", ".swo", ".tmp", ".bak"];

/// Configuration knobs for [`SceneWatcher::spawn`].
///
/// Populated from the `[scene]` section of `ark_config::Config`.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Debounce window. Bursts of accepted events within this window
    /// coalesce into a single callback fire. Matches T-11.6 spec
    /// default of 200 ms.
    pub debounce: Duration,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(200),
        }
    }
}

/// Classify a filesystem path produced by a notify event.
///
/// Returns `true` if the path should trigger a reload; `false` if the
/// watcher should silently drop it (editor temp file).
///
/// The check is purely filename-based — it does not stat the path or
/// consult the watcher target. Callers that want the extra guarantee
/// "only fire when the scene path itself changed" should AND this with
/// a path-equality check against the watched scene file.
pub fn accepts_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };

    // Leading `.#` — emacs lock file. Lock files flash in and out
    // during save; never a real content change.
    if name.starts_with(".#") {
        return false;
    }

    // Trailing `~` — vim / emacs backup.
    if name.ends_with('~') {
        return false;
    }

    // Suffix-based rejection.
    for suffix in EDITOR_SUFFIX_REJECT {
        if name.ends_with(suffix) {
            return false;
        }
    }

    true
}

/// Handle to a spawned scene watcher. Dropping the handle stops the
/// debounce thread and un-registers the inotify/FSEvents callback.
pub struct SceneWatcher {
    /// The scene path being watched (for diagnostics only — the
    /// watcher registers the parent dir internally).
    scene_path: PathBuf,

    /// Shared stop flag. Set to `true` by `Drop`; the debounce thread
    /// checks it on every tick.
    stop: Arc<AtomicBool>,

    /// `notify` watcher guard. Holding keeps the inotify/FSEvents
    /// registration alive. Wrapped in `Option` so `Drop` can take it
    /// without relying on pinning semantics.
    _watcher: Option<notify::RecommendedWatcher>,

    /// Debounce-thread join handle. Dropped after `stop` is set; the
    /// thread exits at the next tick and is joined by the destructor.
    debouncer: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SceneWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneWatcher")
            .field("scene_path", &self.scene_path)
            .field("stopped", &self.stop.load(Ordering::Relaxed))
            .finish()
    }
}

impl SceneWatcher {
    /// Spawn a watcher on `scene_path` with the supplied config.
    ///
    /// `on_change` is invoked once per accepted-change burst (after
    /// the debounce window elapses). It runs on the debounce thread,
    /// so callers should keep the closure short — typically
    /// `move || { let _ = reloader.reload(|| inflight()); }`. Long
    /// work should be spawned onto a tokio runtime or dedicated
    /// worker inside the callback.
    ///
    /// Returns `Err` if the `notify` backend fails to initialise
    /// (rare: usually exhausted inotify watches on Linux) or the
    /// scene path has no parent directory.
    pub fn spawn<F>(
        scene_path: PathBuf,
        config: WatcherConfig,
        on_change: F,
    ) -> Result<Self, WatcherError>
    where
        F: Fn() + Send + 'static,
    {
        // Watch the parent dir, not the file itself — inode-replacement
        // editors (vim default, emacs auto-save) orphan a file-level
        // watch the moment the file is atomically renamed. The parent
        // dir survives.
        let watch_dir = scene_path
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| WatcherError::NoParent {
                path: scene_path.clone(),
            })?;

        let dirty = Arc::new(AtomicBool::new(false));
        let last_change = Arc::new(Mutex::new(Option::<Instant>::None));
        let stop = Arc::new(AtomicBool::new(false));

        // --- notify watcher ---
        let watch_target = scene_path.clone();
        let dirty_cb = dirty.clone();
        let last_change_cb = last_change.clone();
        let watcher_res = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            // Only count events whose paths accept our classifier AND
            // match the watched scene path. Parent-dir watches fire on
            // every sibling file, so we must filter by path.
            let any_match = ev.paths.iter().any(|p| {
                accepts_path(p)
                    && match (p.file_name(), watch_target.file_name()) {
                        (Some(a), Some(b)) => a == b,
                        _ => p == &watch_target,
                    }
            });
            if !any_match {
                return;
            }
            dirty_cb.store(true, Ordering::SeqCst);
            if let Ok(mut guard) = last_change_cb.lock() {
                *guard = Some(Instant::now());
            }
        });

        let mut watcher = watcher_res.map_err(|e| WatcherError::Notify {
            reason: e.to_string(),
        })?;
        watcher
            .watch(&watch_dir, RecursiveMode::NonRecursive)
            .map_err(|e| WatcherError::Notify {
                reason: format!("watch {}: {e}", watch_dir.display()),
            })?;

        // --- debounce thread ---
        let debounce = config.debounce;
        let stop_dbg = stop.clone();
        let dirty_dbg = dirty;
        let last_change_dbg = last_change;
        let scene_path_dbg = scene_path.clone();
        let handle = std::thread::Builder::new()
            .name("scene-watcher-debounce".into())
            .spawn(move || {
                // Tick at a fraction of the debounce so we can fire
                // just-in-time after a burst settles. 50 ms gives us
                // 4x resolution on the default 200 ms debounce and
                // keeps the thread cheap.
                let tick = debounce.checked_div(4).unwrap_or(Duration::from_millis(50));
                loop {
                    if stop_dbg.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(tick);

                    if !dirty_dbg.load(Ordering::SeqCst) {
                        continue;
                    }
                    // Check how long since the last burst-event. If
                    // we're still inside the debounce window, keep
                    // waiting — another event might be en route.
                    let fire = match last_change_dbg.lock() {
                        Ok(guard) => match *guard {
                            Some(t) => t.elapsed() >= debounce,
                            None => false,
                        },
                        Err(_) => false,
                    };
                    if fire {
                        // Clear first so an event arriving mid-callback
                        // still produces a follow-up reload.
                        dirty_dbg.store(false, Ordering::SeqCst);
                        if let Ok(mut guard) = last_change_dbg.lock() {
                            *guard = None;
                        }
                        tracing::debug!(
                            target: "scene::watcher",
                            path = %scene_path_dbg.display(),
                            "firing on_change after debounce"
                        );
                        on_change();
                    }
                }
                tracing::debug!(
                    target: "scene::watcher",
                    path = %scene_path_dbg.display(),
                    "debounce thread exiting"
                );
            })
            .map_err(|e| WatcherError::Thread {
                reason: e.to_string(),
            })?;

        tracing::info!(
            target: "scene::watcher",
            path = %scene_path.display(),
            debounce_ms = debounce.as_millis(),
            "scene watcher spawned"
        );

        Ok(Self {
            scene_path,
            stop,
            _watcher: Some(watcher),
            debouncer: Some(handle),
        })
    }

    /// The scene path this watcher is observing.
    pub fn scene_path(&self) -> &Path {
        &self.scene_path
    }
}

impl Drop for SceneWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Drop the notify watcher first — releases inotify/FSEvents.
        self._watcher.take();
        // Join the debounce thread. It checks `stop` on its next tick
        // (at most `debounce/4` away) and exits.
        if let Some(handle) = self.debouncer.take() {
            let _ = handle.join();
        }
    }
}

/// Failures during [`SceneWatcher::spawn`].
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    /// The scene path has no parent dir — caller passed a raw
    /// filename with no directory component.
    #[error("scene path has no parent directory: {path}")]
    NoParent { path: PathBuf },

    /// `notify` backend returned an error while creating the watcher
    /// or registering the path.
    #[error("notify: {reason}")]
    Notify { reason: String },

    /// The debounce thread failed to spawn (rare; OS resource limit).
    #[error("spawn debounce thread: {reason}")]
    Thread { reason: String },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;
    use tempfile::TempDir;

    // ---- accepts_path classifier ------------------------------------

    #[test]
    fn accepts_plain_kdl_file() {
        assert!(accepts_path(Path::new("/tmp/x/scene.kdl")));
    }

    #[test]
    fn rejects_vim_swap_files() {
        assert!(!accepts_path(Path::new("/tmp/x/.scene.kdl.swp")));
        assert!(!accepts_path(Path::new("/tmp/x/.scene.kdl.swo")));
    }

    #[test]
    fn rejects_emacs_lock_files() {
        // Leading `.#` — emacs lock file.
        assert!(!accepts_path(Path::new("/tmp/x/.#scene.kdl")));
    }

    #[test]
    fn rejects_tilde_backup_files() {
        assert!(!accepts_path(Path::new("/tmp/x/scene.kdl~")));
    }

    #[test]
    fn rejects_rsync_temp_files() {
        assert!(!accepts_path(Path::new("/tmp/x/scene.kdl.tmp")));
    }

    #[test]
    fn rejects_bak_files() {
        assert!(!accepts_path(Path::new("/tmp/x/scene.kdl.bak")));
    }

    #[test]
    fn rejects_path_with_no_file_name() {
        // Root-only paths have no file name.
        assert!(!accepts_path(Path::new("/")));
    }

    // ---- WatcherConfig default --------------------------------------

    #[test]
    fn default_debounce_is_200ms() {
        let cfg = WatcherConfig::default();
        assert_eq!(cfg.debounce, Duration::from_millis(200));
    }

    // ---- full watcher lifecycle -------------------------------------

    /// Spawning a watcher on a file that exists, then modifying the
    /// file, fires `on_change` at least once after the debounce
    /// window elapses.
    #[test]
    fn watcher_fires_on_change() {
        let tmp = TempDir::new().expect("tempdir");
        let scene = tmp.path().join("scene.kdl");
        std::fs::write(&scene, "scene \"v1\" { }\n").expect("seed scene");

        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = count.clone();
        let cfg = WatcherConfig {
            debounce: Duration::from_millis(80),
        };
        let watcher = SceneWatcher::spawn(scene.clone(), cfg, move || {
            count_cb.fetch_add(1, Ordering::SeqCst);
        })
        .expect("spawn watcher");

        // Give the watcher a moment to register.
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(&scene, "scene \"v2\" { }\n").expect("touch");

        // Poll up to 1.5s for the callback.
        let mut fired = false;
        for _ in 0..30 {
            if count.load(Ordering::SeqCst) > 0 {
                fired = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(watcher);
        assert!(fired, "on_change should have fired after modification");
    }

    /// Creating an editor temp file (`.scene.kdl.swp`) in the watched
    /// dir must NOT fire the callback — `accepts_path` filters it
    /// out.
    #[test]
    fn watcher_ignores_editor_temp_files() {
        let tmp = TempDir::new().expect("tempdir");
        let scene = tmp.path().join("scene.kdl");
        std::fs::write(&scene, "scene \"v1\" { }\n").expect("seed scene");

        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = count.clone();
        let cfg = WatcherConfig {
            debounce: Duration::from_millis(80),
        };
        let watcher = SceneWatcher::spawn(scene.clone(), cfg, move || {
            count_cb.fetch_add(1, Ordering::SeqCst);
        })
        .expect("spawn watcher");

        std::thread::sleep(Duration::from_millis(50));
        // Write a vim-swap file next to the scene — the watcher should
        // ignore it.
        let swap = tmp.path().join(".scene.kdl.swp");
        std::fs::write(&swap, "swap content").expect("write swap");

        std::thread::sleep(Duration::from_millis(300));
        drop(watcher);
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "on_change should not have fired for .swp file"
        );
    }

    /// Dropping the watcher must stop the debounce thread.
    #[test]
    fn drop_stops_debounce_thread() {
        let tmp = TempDir::new().expect("tempdir");
        let scene = tmp.path().join("scene.kdl");
        std::fs::write(&scene, "scene \"v1\" { }\n").expect("seed scene");

        let watcher = SceneWatcher::spawn(
            scene.clone(),
            WatcherConfig {
                debounce: Duration::from_millis(50),
            },
            || {},
        )
        .expect("spawn watcher");

        // `drop` joins the debounce thread internally — if it hangs,
        // the test times out. A successful drop is the assertion.
        drop(watcher);
    }

    /// A rapid burst of writes should coalesce into a single
    /// `on_change` fire thanks to debounce. We don't assert exactly
    /// `== 1` because timing on loaded CI runners is racy; we assert
    /// "fewer fires than writes."
    #[test]
    fn burst_of_writes_coalesces_via_debounce() {
        let tmp = TempDir::new().expect("tempdir");
        let scene = tmp.path().join("scene.kdl");
        std::fs::write(&scene, "scene \"v0\" { }\n").expect("seed scene");

        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = count.clone();
        let cfg = WatcherConfig {
            debounce: Duration::from_millis(120),
        };
        let watcher = SceneWatcher::spawn(scene.clone(), cfg, move || {
            count_cb.fetch_add(1, Ordering::SeqCst);
        })
        .expect("spawn watcher");

        std::thread::sleep(Duration::from_millis(50));
        // Five quick writes inside the debounce window.
        for i in 0..5 {
            std::fs::write(&scene, format!("scene \"v{i}\" {{ }}\n")).expect("touch");
            std::thread::sleep(Duration::from_millis(15));
        }
        // Wait for debounce + slack.
        std::thread::sleep(Duration::from_millis(400));
        let fires = count.load(Ordering::SeqCst);
        drop(watcher);
        assert!(fires >= 1, "at least one fire expected, got {fires}");
        assert!(
            fires < 5,
            "burst should coalesce; expected < 5 fires, got {fires}"
        );
    }
}
