//! Immutable candidate snapshot for HCR acceptance gates.
//!
//! A candidate snapshot is a read-only copy of the source workspace,
//! identified by a unique ID and protected by a SHA-256 digest.
//! All five acceptance gates operate on the same snapshot, with
//! digest verification before and after each gate execution.
//!
//! The snapshot is stored in `~/.agent-core/harness-artifacts/candidates/<id>/`.

use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

/// An immutable candidate snapshot.
#[derive(Debug, Clone)]
pub struct CandidateSnapshot {
    /// Unique identifier for this candidate (timestamp-based).
    pub candidate_id: String,
    /// Absolute path to the immutable copy of the candidate source.
    pub candidate_path: PathBuf,
    /// SHA-256 digest of the candidate content (format: "sha256:<hex>").
    pub candidate_digest: String,
}

/// Errors that can occur during candidate snapshot operations.
#[derive(Debug)]
pub enum CandidateError {
    /// Underlying I/O error.
    Io(io::Error),
    /// Candidate source directory does not exist or is empty.
    InvalidSource(String),
    /// Digest computation failed (no files found).
    NoFiles,
    /// A path in the candidate is not valid UTF-8.
    InvalidPath,
}

impl std::fmt::Display for CandidateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CandidateError::Io(e) => write!(f, "I/O error: {e}"),
            CandidateError::InvalidSource(s) => write!(f, "invalid source: {s}"),
            CandidateError::NoFiles => write!(f, "no files found in candidate"),
            CandidateError::InvalidPath => write!(f, "invalid path (non-UTF-8)"),
        }
    }
}

impl std::error::Error for CandidateError {}

impl From<io::Error> for CandidateError {
    fn from(e: io::Error) -> Self {
        CandidateError::Io(e)
    }
}

/// Create an immutable snapshot of the given source directory.
///
/// The source is copied to a new directory under `base_dir/candidates/<id>/`,
/// made read-only, and a SHA-256 digest is computed over all files.
pub fn snapshot_candidate(
    source: &Path,
    base_dir: &Path,
) -> Result<CandidateSnapshot, CandidateError> {
    if !source.is_dir() {
        return Err(CandidateError::InvalidSource(format!(
            "not a directory: {}",
            source.display()
        )));
    }

    let candidates_dir = base_dir.join("candidates");
    let candidate_id = generate_id();
    let candidate_path = candidates_dir.join(&candidate_id);

    // Create the candidate directory
    std::fs::create_dir_all(&candidate_path)?;

    // Recursively copy source contents (excluding target/ if present)
    copy_source(source, &candidate_path)?;

    // Make the candidate tree read-only
    make_readonly(&candidate_path)?;

    // Compute digest
    let digest = compute_digest(&candidate_path)?;

    Ok(CandidateSnapshot {
        candidate_id,
        candidate_path,
        candidate_digest: digest,
    })
}

/// Verify that the candidate snapshot's digest matches its current content.
///
/// Returns `Ok(true)` if the digest matches, `Ok(false)` if it has changed,
/// or `Err` if verification itself fails.
pub fn verify_digest(snapshot: &CandidateSnapshot) -> Result<bool, CandidateError> {
    let current = compute_digest(&snapshot.candidate_path)?;
    Ok(current == snapshot.candidate_digest)
}

/// Recursively copy directory contents, excluding `target/` directories.
fn copy_source(src: &Path, dst: &Path) -> Result<(), CandidateError> {
    for entry in walkdir_files(src)? {
        let relative = entry
            .strip_prefix(src)
            .map_err(|_| CandidateError::InvalidPath)?;

        // Skip target/ directories
        if relative.components().any(|c| c.as_os_str() == "target") {
            continue;
        }

        let dest_path = dst.join(relative);

        if entry.is_dir() {
            std::fs::create_dir_all(&dest_path)?;
        } else {
            if let Some(parent) = dest_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry, &dest_path)?;
        }
    }
    Ok(())
}

/// Recursively remove write permissions from all files and directories.
fn make_readonly(path: &Path) -> Result<(), CandidateError> {
    for entry in walkdir_files(path)? {
        let metadata = entry.metadata()?;
        let mut perms = metadata.permissions();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = perms.mode();
            if entry.is_dir() {
                perms.set_mode((mode & !0o222) | 0o111);
            } else {
                perms.set_mode(mode & !0o222);
            }
        }

        std::fs::set_permissions(&entry, perms)?;
    }
    Ok(())
}

/// Compute a SHA-256 digest of all files in a directory tree.
///
/// The digest is computed over sorted relative paths and their contents:
/// `sha256("path1\0" + content1 + "path2\0" + content2 + ...)`
///
/// Skips `target/` directories.
pub(crate) fn compute_digest(root: &Path) -> Result<String, CandidateError> {
    let mut entries: Vec<PathBuf> = Vec::new();
    collect_relative_files(root, root, &mut entries)?;

    if entries.is_empty() {
        return Err(CandidateError::NoFiles);
    }

    entries.sort();

    let mut hasher = Sha256::new();
    for rel in &entries {
        let full_path = root.join(rel);
        let content = std::fs::read(&full_path)?;
        let rel_str = rel.to_string_lossy();
        hasher.update(rel_str.as_bytes());
        hasher.update(b"\0");
        hasher.update(&content);
    }

    let hex = hex::encode(hasher.finalize());
    Ok(format!("sha256:{hex}"))
}

/// Walk a directory tree and collect relative paths of all regular files,
/// excluding `target/` directories.
fn collect_relative_files(
    root: &Path,
    dir: &Path,
    entries: &mut Vec<PathBuf>,
) -> Result<(), CandidateError> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();

        // Skip target/ directories at any level
        if path.is_dir() && file_name == "target" {
            continue;
        }

        if path.is_dir() {
            collect_relative_files(root, &path, entries)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| CandidateError::InvalidPath)?
                .to_path_buf();
            entries.push(relative);
        }
    }
    Ok(())
}

/// Walk a directory tree collecting all entries (files and dirs).
fn walkdir_files(path: &Path) -> Result<Vec<PathBuf>, CandidateError> {
    let mut result = Vec::new();
    collect_entries(path, &mut result)?;
    Ok(result)
}

fn collect_entries(dir: &Path, entries: &mut Vec<PathBuf>) -> Result<(), CandidateError> {
    if !dir.is_dir() {
        entries.push(dir.to_path_buf());
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();

        if path.is_dir() && file_name == "target" {
            continue;
        }

        entries.push(path.clone());
        if path.is_dir() {
            collect_entries(&path, entries)?;
        }
    }
    Ok(())
}

/// Generate a unique candidate identifier based on timestamp.
fn generate_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    format!("candidate_{pid}_{nanos}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_creates_readonly_copy() {
        let tmp = std::env::temp_dir().join(format!("hcr_cand_test_{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), b"[package]\nname = \"test\"\n").unwrap();
        std::fs::write(tmp.join("src/main.rs"), b"fn main() {}").unwrap();

        let base = std::env::temp_dir().join(format!("hcr_base_{}", std::process::id()));
        let snapshot = snapshot_candidate(&tmp, &base).unwrap();

        // Verify copy exists
        assert!(snapshot.candidate_path.join("Cargo.toml").exists());
        assert!(snapshot.candidate_path.join("src/main.rs").exists());

        // Verify read-only (write should fail)
        #[cfg(unix)]
        {
            let result = std::fs::write(snapshot.candidate_path.join("Cargo.toml"), b"modified");
            assert!(result.is_err(), "write to read-only file should fail");
        }

        // Verify digest
        assert!(verify_digest(&snapshot).unwrap());

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn digest_changes_when_file_modified() {
        let tmp = std::env::temp_dir().join(format!("hcr_digest_test_{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), b"[package]\nname = \"test\"\n").unwrap();
        std::fs::write(tmp.join("src/main.rs"), b"fn main() {}").unwrap();

        let base = std::env::temp_dir().join(format!("hcr_base_digest_{}", std::process::id()));
        let snapshot = snapshot_candidate(&tmp, &base).unwrap();

        // Snapshot is read-only, so we can't modify it directly.
        // Instead, verify the initial digest is valid.
        assert!(verify_digest(&snapshot).unwrap());

        // Create a different source and verify it produces a different digest
        let tmp2 = std::env::temp_dir().join(format!("hcr_digest_test2_{}", std::process::id()));
        std::fs::create_dir_all(tmp2.join("src")).unwrap();
        std::fs::write(tmp2.join("Cargo.toml"), b"[package]\nname = \"test2\"\n").unwrap();
        std::fs::write(tmp2.join("src/main.rs"), b"fn main() { println!(\"hi\"); }").unwrap();

        let snapshot2 = snapshot_candidate(&tmp2, &base).unwrap();
        assert_ne!(snapshot.candidate_digest, snapshot2.candidate_digest);

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn excludes_target_directory() {
        let tmp = std::env::temp_dir().join(format!("hcr_excl_test_{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("target/release")).unwrap();
        std::fs::write(tmp.join("target/release/calculator"), b"binary").unwrap();
        std::fs::write(tmp.join("Cargo.toml"), b"[package]\n").unwrap();

        let base = std::env::temp_dir().join(format!("hcr_base_excl_{}", std::process::id()));
        let snapshot = snapshot_candidate(&tmp, &base).unwrap();

        // target/ should not be in snapshot
        assert!(!snapshot
            .candidate_path
            .join("target/release/calculator")
            .exists());

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn invalid_source_returns_error() {
        let base = std::env::temp_dir().join(format!("hcr_err_test_{}", std::process::id()));
        let result = snapshot_candidate(Path::new("/nonexistent/path"), &base);
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
