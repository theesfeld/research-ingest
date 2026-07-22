//! Obsidian vault layout helpers.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use regex::Regex;
use walkdir::WalkDir;

/// Standard paths under the vault root.
#[derive(Debug, Clone)]
pub struct VaultPaths {
    pub root: PathBuf,
}

impl VaultPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn incoming(&self) -> PathBuf {
        self.root.join("raw").join("incoming")
    }

    pub fn processed(&self) -> PathBuf {
        self.root.join("raw").join("processed")
    }

    pub fn extracts(&self) -> PathBuf {
        self.root.join("raw").join("extracts")
    }

    pub fn projects(&self) -> PathBuf {
        self.root.join("wiki").join("projects")
    }

    pub fn project_dir(&self, slug: &str) -> PathBuf {
        self.projects().join(slug)
    }

    pub fn index_md(&self) -> PathBuf {
        self.root.join("index.md")
    }

    pub fn map_of_content(&self) -> PathBuf {
        self.root.join("map-of-content.md")
    }

    pub fn inbox_md(&self) -> PathBuf {
        self.root.join("wiki").join("inbox.md")
    }

    /// Create the required directory tree and seed index files if missing.
    pub fn ensure_layout(&self) -> Result<()> {
        for d in [
            self.incoming(),
            self.processed(),
            self.extracts(),
            self.projects(),
            self.root.join("wiki"),
        ] {
            fs::create_dir_all(&d).with_context(|| format!("create {}", d.display()))?;
        }

        if !self.index_md().exists() {
            fs::write(
                self.index_md(),
                "# Knowledge index\n\n\
                 This file lists research projects. The ingest tool updates this file.\n\n\
                 ## Projects\n\n\
                 (none yet)\n",
            )?;
        }
        if !self.map_of_content().exists() {
            fs::write(
                self.map_of_content(),
                "# Map of content\n\n\
                 High-level map of the vault. The ingest tool updates project links here.\n\n\
                 ## Projects\n\n\
                 (none yet)\n",
            )?;
        }
        if !self.inbox_md().exists() {
            fs::write(
                self.inbox_md(),
                "# Inbox\n\n\
                 Notes that are not yet assigned to a project appear here.\n",
            )?;
        }
        // Marker so users see the drop target in Obsidian.
        let readme = self.incoming().join("README.md");
        if !readme.exists() {
            fs::write(
                readme,
                "# Incoming\n\n\
                 Drop research files here, or use **Send to Grok Research** in the browser.\n\n\
                 The `research-ingest` watcher processes new files and moves them to `raw/processed/`.\n",
            )?;
        }
        Ok(())
    }

    /// List project slugs (directory names under wiki/projects).
    pub fn list_project_slugs(&self) -> Result<Vec<String>> {
        let mut slugs = Vec::new();
        let dir = self.projects();
        if !dir.exists() {
            return Ok(slugs);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    if !name.starts_with('.') {
                        slugs.push(name.to_string());
                    }
                }
            }
        }
        slugs.sort();
        Ok(slugs)
    }

    /// Ensure a project folder and its root note exist.
    pub fn ensure_project(&self, slug: &str, title: &str) -> Result<PathBuf> {
        let dir = self.project_dir(slug);
        fs::create_dir_all(&dir)?;
        let root_note = dir.join("_project.md");
        if !root_note.exists() {
            let body = format!(
                "---\ntitle: \"{title}\"\nslug: \"{slug}\"\ncreated: {}\n---\n\n\
                 # {title}\n\n\
                 ## Sources\n\n\
                 ## Notes\n\n",
                Utc::now().format("%Y-%m-%d")
            );
            fs::write(&root_note, body)?;
        }
        self.upsert_project_in_indexes(slug, title)?;
        Ok(dir)
    }

    fn upsert_project_in_indexes(&self, slug: &str, title: &str) -> Result<()> {
        let link = format!("- [[{slug}/_project|{title}]]");
        upsert_bullet_line(&self.index_md(), "## Projects", &link)?;
        upsert_bullet_line(&self.map_of_content(), "## Projects", &link)?;
        Ok(())
    }

    /// Move a finished source into raw/processed, preserving basename + job id prefix.
    pub fn move_to_processed(&self, source: &Path, job_id: &str) -> Result<PathBuf> {
        self.ensure_layout()?;
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "item.bin".into());
        let dest = self.processed().join(format!("{job_id}_{name}"));
        if source.exists() {
            fs::rename(source, &dest)
                .or_else(|_| {
                    fs::copy(source, &dest)?;
                    fs::remove_file(source)?;
                    Ok::<(), std::io::Error>(())
                })
                .with_context(|| format!("move {} → {}", source.display(), dest.display()))?;
        }
        Ok(dest)
    }

    /// Search markdown notes for a case-insensitive substring. Returns relative paths.
    pub fn search_notes(&self, query: &str, limit: usize) -> Result<Vec<(PathBuf, String)>> {
        let q = query.to_ascii_lowercase();
        let mut hits = Vec::new();
        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case("md"))
                    .unwrap_or(false)
            })
        {
            let path = entry.path();
            // Skip raw bulk extracts if huge; still allow wiki.
            let text = match fs::read_to_string(path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if text.to_ascii_lowercase().contains(&q) {
                let rel = path.strip_prefix(&self.root).unwrap_or(path).to_path_buf();
                let snippet = first_snippet(&text, &q, 160);
                hits.push((rel, snippet));
                if hits.len() >= limit {
                    break;
                }
            }
        }
        Ok(hits)
    }
}

fn first_snippet(text: &str, q_lower: &str, max: usize) -> String {
    let lower = text.to_ascii_lowercase();
    if let Some(i) = lower.find(q_lower) {
        let start = i.saturating_sub(40);
        let end = (i + q_lower.len() + 80).min(text.len());
        let mut s: String = text[start..end].chars().take(max).collect();
        s = s.replace('\n', " ");
        return s;
    }
    text.chars().take(max).collect()
}

fn upsert_bullet_line(path: &Path, section_heading: &str, bullet: &str) -> Result<()> {
    let mut body = if path.exists() {
        fs::read_to_string(path)?
    } else {
        format!("{section_heading}\n\n")
    };

    if body.contains(bullet) {
        return Ok(());
    }

    // Replace placeholder.
    body = body.replace("(none yet)\n", "");

    if let Some(pos) = body.find(section_heading) {
        let after = pos + section_heading.len();
        // Insert after heading line.
        let insert_at = body[after..]
            .find('\n')
            .map(|i| after + i + 1)
            .unwrap_or(body.len());
        body.insert_str(insert_at, &format!("{bullet}\n"));
    } else {
        body.push_str(&format!("\n{section_heading}\n\n{bullet}\n"));
    }
    fs::write(path, body)?;
    Ok(())
}

/// Turn free text into a filesystem-safe project slug.
pub fn slugify(input: &str) -> String {
    let lower = input.trim().to_ascii_lowercase();
    let re = Regex::new(r"[^a-z0-9]+").expect("regex");
    let s = re.replace_all(&lower, "-");
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "inbox".into()
    } else {
        s.chars().take(64).collect()
    }
}

/// SHA-256 hex of file bytes.
pub fn file_sha256(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).with_context(|| format!("hash {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Elevator Modernization!"), "elevator-modernization");
        assert_eq!(slugify("  "), "inbox");
    }

    #[test]
    fn ensure_layout_creates_tree() {
        let dir = std::env::temp_dir().join(format!("ri-vault-{}", uuid::Uuid::new_v4()));
        let v = VaultPaths::new(&dir);
        v.ensure_layout().unwrap();
        assert!(v.incoming().is_dir());
        assert!(v.index_md().is_file());
        let _ = fs::remove_dir_all(dir);
    }
}
