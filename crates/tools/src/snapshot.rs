// ZeroZero — file rewind shadow snapshot store (B /).
//
// Before a mutating file tool (write/edit) overwrites an existing file,
// we capture its current content into an in-process store keyed by path.
// The user can later revert via `/rewind <path>` (TUI) or `zz rewind <path>`
// (CLI), which restores the captured content and clears the entry.
//
// Design note: a process-global OnceLock<Mutex<SnapshotStore>> keeps the
// wiring minimal (tools call free functions; no ctx plumbing needed). This
// is "Plan B" from the backlog — single-step rewind, not full history.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Stores the pre-mutation content of files, keyed by absolute path.
pub struct SnapshotStore {
    map: HashMap<PathBuf, Vec<u8>>,
}

impl SnapshotStore {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Capture the current content of `path` if it exists on disk.
    /// Overwrites any existing snapshot for that path (single-step rewind).
    pub fn capture(&mut self, path: &Path) {
        if let Ok(bytes) = std::fs::read(path) {
            self.map.insert(path.to_path_buf(), bytes);
        }
    }

    /// Restore the captured content for `path`, then clear the entry.
    /// Errors if no snapshot exists (file was never captured / already
    /// rewound).
    pub fn rewind(&mut self, path: &Path) -> anyhow::Result<()> {
        match self.map.remove(path) {
            Some(bytes) => {
                std::fs::write(path, bytes)
                    .map_err(|e| anyhow::anyhow!("rewind write failed: {e}"))?;
                Ok(())
            }
            None => Err(anyhow::anyhow!(
                "no snapshot for {} — file was never modified this session",
                path.display()
            )),
        }
    }

    /// True if a snapshot exists for `path`.
    pub fn has(&self, path: &Path) -> bool {
        self.map.contains_key(path)
    }

    /// Clear all snapshots (used by tests to reset global state).
    pub fn clear(&mut self) {
        self.map.clear();
    }
}

static STORE: OnceLock<Mutex<SnapshotStore>> = OnceLock::new();

/// Global handle to the snapshot store.
pub fn store() -> &'static Mutex<SnapshotStore> {
    STORE.get_or_init(|| Mutex::new(SnapshotStore::new()))
}

/// Capture `path`'s current content (no-op if file does not exist).
pub fn capture(path: &Path) {
    store().lock().unwrap().capture(path);
}

/// Restore `path` from its snapshot, clearing the entry.
pub fn rewind(path: &Path) -> anyhow::Result<()> {
    store().lock().unwrap().rewind(path)
}

/// Whether a snapshot exists for `path`.
pub fn has(path: &Path) -> bool {
    store().lock().unwrap().has(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpfile() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"original").unwrap();
        drop(f);
        (dir, path)
    }

    // AC-1: capture_before_write
    #[test]
    fn capture_before_write() {
        let (_dir, path) = tmpfile();
        assert!(!has(&path));
        capture(&path);
        assert!(has(&path));
    }

    // AC-2: rewind_restores_previous_content
    #[test]
    fn rewind_restores_previous_content() {
        let (_dir, path) = tmpfile();
        capture(&path);
        // Mutate the file (simulating a write/edit tool).
        std::fs::write(&path, b"mutated-content").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"mutated-content");
        // Rewind restores the captured content.
        rewind(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert!(!has(&path), "snapshot cleared after rewind");
    }

    // AC-5: rewind_no_snapshot_errors
    #[test]
    fn rewind_no_snapshot_errors() {
        let (_dir, path) = tmpfile();
        let err = rewind(&path);
        assert!(err.is_err(), "rewind with no snapshot must error");
        assert!(err.unwrap_err().to_string().contains("no snapshot"));
    }
}
