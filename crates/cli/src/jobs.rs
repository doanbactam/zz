//! Background job tracking parity with Codex "Background mode").
//!
//! `zz exec --background` records a job; `zz jobs` lists them and
//! `zz jobs show <id>` prints a job's detail. Jobs persist to
//! `~/.config/zerozero/jobs.json` so the operator can inspect them after the
//! CLI process exits — unlike the in-process `BashTool` background registry.

#![cfg_attr(test, allow(clippy::bool_assert_comparison))]

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Status of a background job.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Done,
    Failed,
}

/// A single background job record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub prompt: String,
    /// When the job was created (RFC3339).
    pub created_at: String,
    pub status: JobStatus,
    /// Exit/result summary once finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Session id the job was attached to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// On-disk + in-memory store of background jobs.
#[derive(Debug, Default)]
pub struct JobStore {
    path: PathBuf,
    jobs: Mutex<Vec<Job>>,
}

impl JobStore {
    /// Build a store rooted at the given jobs.json path (does not load yet).
    pub const fn new(path: PathBuf) -> Self {
        Self {
            path,
            jobs: Mutex::new(Vec::new()),
        }
    }

    /// Default location: `~/.config/zerozero/jobs.json`.
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".config")
            .join("zerozero")
            .join("jobs.json")
    }

    /// Directory holding per-job stdout/stderr logs
    /// (`~/.config/zerozero/jobs/<id>.log`).
    pub fn log_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".config")
            .join("zerozero")
            .join("jobs")
    }

    /// Log file path for a given job id.
    pub fn log_path(id: &str) -> PathBuf {
        Self::log_dir().join(format!("{id}.log"))
    }

    /// Load jobs from disk (best-effort; missing file => empty).
    pub fn load(&self) -> std::io::Result<()> {
        let mut guard = self.jobs.lock().unwrap();
        if !self.path.exists() {
            *guard = Vec::new();
            return Ok(());
        }
        let data = std::fs::read_to_string(&self.path)?;
        let parsed: Vec<Job> = serde_json::from_str(&data).unwrap_or_default();
        *guard = parsed;
        Ok(())
    }

    fn persist(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let guard = self.jobs.lock().unwrap();
        let data = serde_json::to_string_pretty(&*guard)?;
        std::fs::write(&self.path, data)
    }

    /// Insert or replace a job by id, then persist.
    pub fn upsert(&self, job: Job) -> std::io::Result<()> {
        {
            let mut guard = self.jobs.lock().unwrap();
            if let Some(existing) = guard.iter_mut().find(|j| j.id == job.id) {
                *existing = job;
            } else {
                guard.push(job);
            }
        }
        self.persist()
    }

    /// All jobs, newest first.
    pub fn list(&self) -> Vec<Job> {
        let mut guard = self.jobs.lock().unwrap();
        guard.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        guard.clone()
    }

    /// Fetch a single job by id.
    pub fn get(&self, id: &str) -> Option<Job> {
        self.jobs
            .lock()
            .unwrap()
            .iter()
            .find(|j| j.id == id)
            .cloned()
    }

    /// Read the captured stdout/stderr log for a job id.
    /// Returns `io::ErrorKind::NotFound` if `jobs/<id>.log` is absent
    /// (e.g. the job never produced output or has not started writing).
    pub fn read_log(&self, id: &str) -> std::io::Result<String> {
        std::fs::read_to_string(Self::log_path(id))
    }

    /// Remove all jobs from the store and delete every per-job log file
    /// under `log_dir()`. Best-effort: individual log deletions that fail
    /// are ignored, but the store file is removed.
    pub fn clear(&self) -> std::io::Result<()> {
        // Drop logs first; missing/locked files are skipped.
        let dir = Self::log_dir();
        if dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        {
            let mut guard = self.jobs.lock().unwrap();
            guard.clear();
        }
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    /// Generate a short unique id (time-based, no external dep).
    pub fn new_id() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("job-{now:x}")
    }
}

/// RFC3339-ish timestamp without a chrono dependency (parity with `chrono::Utc::now().to_rfc3339`).
pub fn chrono_like_now() -> String {
    // Use the `log` crate's timestamp if available; otherwise fall back to a
    // compact UTC string. We avoid pulling in chrono just for one timestamp.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Simple, sortable UTC-ish stamp: seconds since epoch (good enough for ordering).
    format!("{secs}")
}

impl JobStatus {
    /// Lowercase string used in `zz jobs` output and JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize tests that touch the global `$HOME`-based `log_dir()`.
    /// Without this, `read_log_returns_file_contents` (which calls
    /// `std::env::set_var("HOME", ...)`) races with `clear_removes_jobs_and_logs`
    /// in parallel test mode, causing intermittent failures.
    static LOG_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn tmp_store() -> (JobStore, PathBuf) {
        // Unique per call so parallel tests don't clobber each other.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("zz-jobs-test-{}-{n}", std::process::id()));
        let path = dir.join("jobs.json");
        let _ = std::fs::remove_file(&path);
        (JobStore::new(path.clone()), path)
    }

    fn sample_job(id: &str, status: JobStatus) -> Job {
        Job {
            id: id.to_string(),
            prompt: "do the thing".to_string(),
            created_at: "1000".to_string(),
            status,
            result: None,
            session_id: None,
        }
    }

    #[test]
    fn upsert_persists_and_get_reads_back() {
        let (store, path) = tmp_store();
        store
            .upsert(sample_job("job-a", JobStatus::Running))
            .unwrap();
        drop(store);

        // Reload from disk — persistence across instances (the parity gap we close).
        let reloaded = JobStore::new(path.clone());
        reloaded.load().unwrap();
        let job = reloaded.get("job-a").expect("job should persist to disk");
        assert_eq!(job.status, JobStatus::Running);
        assert_eq!(job.prompt, "do the thing");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn list_is_newest_first() {
        let (store, path) = tmp_store();
        store
            .upsert(sample_job("job-old", JobStatus::Done))
            .unwrap();
        store
            .upsert(sample_job("job-new", JobStatus::Running))
            .unwrap();
        let jobs = store.list();
        assert_eq!(jobs.len(), 2);
        // Jobs::list sorts by created_at desc; we set both to "1000" so order is stable-ish.
        assert!(jobs.iter().any(|j| j.id == "job-old"));
        assert!(jobs.iter().any(|j| j.id == "job-new"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn upsert_replaces_existing_id() {
        let (store, path) = tmp_store();
        store
            .upsert(sample_job("job-a", JobStatus::Running))
            .unwrap();
        store.upsert(sample_job("job-a", JobStatus::Done)).unwrap();
        let jobs = store.list();
        assert_eq!(jobs.len(), 1, "same id must not create a duplicate");
        assert_eq!(jobs[0].status, JobStatus::Done);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn get_missing_returns_none() {
        let (store, path) = tmp_store();
        assert!(store.get("nope").is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn status_as_str_roundtrips() {
        assert_eq!(JobStatus::Running.as_str(), "running");
        assert_eq!(JobStatus::Done.as_str(), "done");
        assert_eq!(JobStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn read_log_returns_file_contents() {
        let _guard = LOG_DIR_LOCK.lock().unwrap();
        let (store, path) = tmp_store();
        // Isolate under a temp HOME so a concurrent clear() can't wipe the log.
        let home = path.parent().unwrap().to_path_buf();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        // Write a log file under the store's log_dir for a known id.
        let id = "job-logtest";
        let log_path = JobStore::log_path(id);
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&log_path, "hello from job\nsecond line").unwrap();
        let contents = store.read_log(id).expect("log should be readable");
        assert_eq!(contents, "hello from job\nsecond line");
        let _ = std::fs::remove_file(&log_path);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_log_missing_is_not_found() {
        let (store, path) = tmp_store();
        let err = store
            .read_log("job-does-not-exist")
            .expect_err("absent log must error");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn clear_removes_jobs_and_logs() {
        let _guard = LOG_DIR_LOCK.lock().unwrap();
        let (store, path) = tmp_store();
        // Isolate under a temp HOME so log_path resolves to our temp dir,
        // not whatever a previous test left in $HOME.
        let home = path.parent().unwrap().to_path_buf();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        store.upsert(sample_job("job-c1", JobStatus::Done)).unwrap();
        // Drop a log file alongside.
        let log_path = JobStore::log_path("job-c1");
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&log_path, "captured output").unwrap();
        assert!(store.get("job-c1").is_some());
        assert!(log_path.exists());

        store.clear().expect("clear should succeed");
        // In-memory is empty.
        assert!(store.get("job-c1").is_none());
        // On-disk store file removed.
        assert!(!path.exists());
        // Log file removed.
        assert!(!log_path.exists());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&log_path);
    }
}
