//! Shared types, vault layout, config, and job queue for research-ingest.

pub mod config;
pub mod job;
pub mod queue;
pub mod vault;

pub use config::{AiBackend, Config, GrokSessionConfig};
pub use job::{ContentKind, IngestJob, JobStatus};
pub use queue::JobQueue;
pub use vault::VaultPaths;
