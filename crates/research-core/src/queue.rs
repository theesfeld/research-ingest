//! JSON-lines job queue on disk.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::config::state_dir;
use crate::job::{IngestJob, JobStatus};

/// Append-only style store: one JSON file per job + index of active ids.
#[derive(Debug, Clone)]
pub struct JobQueue {
    root: PathBuf,
}

impl JobQueue {
    pub fn open_default() -> Result<Self> {
        Self::open(state_dir().join("queue"))
    }

    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("jobs"))
            .with_context(|| format!("create queue {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn job_path(&self, id: Uuid) -> PathBuf {
        self.root.join("jobs").join(format!("{id}.json"))
    }

    pub fn put(&self, job: &IngestJob) -> Result<()> {
        let path = self.job_path(job.id);
        let text = serde_json::to_string_pretty(job).context("serialize job")?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Result<Option<IngestJob>> {
        let path = self.job_path(id);
        if !path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path)?;
        Ok(Some(serde_json::from_str(&text)?))
    }

    pub fn list(&self) -> Result<Vec<IngestJob>> {
        let mut out = Vec::new();
        let dir = self.root.join("jobs");
        if !dir.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            match serde_json::from_str::<IngestJob>(&text) {
                Ok(j) => out.push(j),
                Err(e) => tracing::warn!("skip corrupt job {}: {e}", path.display()),
            }
        }
        out.sort_by_key(|j| j.created_at);
        Ok(out)
    }

    pub fn list_by_status(&self, status: JobStatus) -> Result<Vec<IngestJob>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|j| j.status == status)
            .collect())
    }

    pub fn next_pending(&self) -> Result<Option<IngestJob>> {
        let mut pending = self.list_by_status(JobStatus::Pending)?;
        // Also continue jobs left in mid-pipeline after a crash.
        let mut mid = self.list_by_status(JobStatus::Extracting)?;
        mid.extend(self.list_by_status(JobStatus::AwaitingAi)?);
        mid.extend(self.list_by_status(JobStatus::Writing)?);
        mid.append(&mut pending);
        Ok(mid.into_iter().next())
    }

    /// Record that this source hash was already processed (dedupe).
    pub fn mark_hash_seen(&self, sha256: &str) -> Result<()> {
        let path = self.root.join("seen_hashes.txt");
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{sha256}")?;
        Ok(())
    }

    pub fn hash_seen(&self, sha256: &str) -> Result<bool> {
        let path = self.root.join("seen_hashes.txt");
        if !path.exists() {
            return Ok(false);
        }
        let f = File::open(&path)?;
        for line in BufReader::new(f).lines() {
            if line? == sha256 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Find an open job for this source path.
    pub fn find_by_source(&self, source: &Path) -> Result<Option<IngestJob>> {
        let canon = source.to_path_buf();
        Ok(self.list()?.into_iter().find(|j| {
            j.source_path == canon && !matches!(j.status, JobStatus::Done | JobStatus::Skipped)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_list() {
        let dir = std::env::temp_dir().join(format!("ri-queue-{}", Uuid::new_v4()));
        let q = JobQueue::open(&dir).unwrap();
        let job = IngestJob::new(PathBuf::from("/tmp/a.md"));
        let id = job.id;
        q.put(&job).unwrap();
        let loaded = q.get(id).unwrap().unwrap();
        assert_eq!(loaded.id, id);
        assert_eq!(q.list().unwrap().len(), 1);
        let _ = fs::remove_dir_all(dir);
    }
}
