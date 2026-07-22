//! Ingest job records.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Extracting,
    AwaitingAi,
    Writing,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Text,
    Markdown,
    Html,
    Pdf,
    Image,
    Video,
    Audio,
    UrlClip,
    Unknown,
}

impl ContentKind {
    pub fn from_path(path: &std::path::Path) -> Self {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match ext.as_str() {
            "txt" | "text" | "log" => Self::Text,
            "md" | "markdown" => Self::Markdown,
            "html" | "htm" => Self::Html,
            "pdf" => Self::Pdf,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "tif" | "tiff" | "bmp" => Self::Image,
            "mp4" | "webm" | "mkv" | "mov" | "avi" => Self::Video,
            "mp3" | "wav" | "m4a" | "ogg" | "flac" => Self::Audio,
            "json" => Self::UrlClip, // browser payload often JSON envelope
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestJob {
    pub id: Uuid,
    pub status: JobStatus,
    pub source_path: PathBuf,
    pub kind: ContentKind,
    pub content_sha256: Option<String>,
    pub title: Option<String>,
    pub source_url: Option<String>,
    pub extracted_text_path: Option<PathBuf>,
    pub project_slug: Option<String>,
    pub note_path: Option<PathBuf>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Optional browser metadata blob (JSON string).
    pub metadata_json: Option<String>,
}

impl IngestJob {
    pub fn new(source_path: PathBuf) -> Self {
        let now = Utc::now();
        let kind = ContentKind::from_path(&source_path);
        Self {
            id: Uuid::new_v4(),
            status: JobStatus::Pending,
            source_path,
            kind,
            content_sha256: None,
            title: None,
            source_url: None,
            extracted_text_path: None,
            project_slug: None,
            note_path: None,
            error: None,
            created_at: now,
            updated_at: now,
            metadata_json: None,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    pub fn fail(&mut self, err: impl Into<String>) {
        self.status = JobStatus::Failed;
        self.error = Some(err.into());
        self.touch();
    }

    pub fn set_status(&mut self, status: JobStatus) {
        self.status = status;
        self.touch();
    }
}
