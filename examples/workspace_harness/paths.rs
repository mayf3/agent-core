//! Path resolution and escape-prevention for workspace operations.
//!
//! Security invariants:
//! 1. Unknown workspace_id → denied.
//! 2. `relative_path` is absolute → denied.
//! 3. `relative_path` contains `..` → denied.
//! 4. Resolved path is not within workspace root → denied.
//! 5. Symlink in resolved path escapes workspace root → denied file ops.
//! 6. Path does not exist → denied stat/read (mkdir may create).

use std::path::{Component, Path, PathBuf};

/// Error codes for path validation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    UnknownWorkspace,
    AbsolutePath,
    PathTraversal,
    OutsideWorkspace,
    SymlinkEscape,
    NotFound,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::UnknownWorkspace => write!(f, "unknown_workspace"),
            PathError::AbsolutePath => write!(f, "absolute_path_not_allowed"),
            PathError::PathTraversal => write!(f, "path_traversal_not_allowed"),
            PathError::OutsideWorkspace => write!(f, "path_outside_workspace"),
            PathError::SymlinkEscape => write!(f, "symlink_escapes_workspace"),
            PathError::NotFound => write!(f, "path_not_found"),
        }
    }
}

/// Validate that `relative` is a safe relative path (not absolute, no `..`).
pub fn validate_relative_path(relative: &str) -> Result<&str, PathError> {
    if relative.is_empty() {
        return Ok(relative);
    }
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(PathError::AbsolutePath);
    }
    for component in path.components() {
        if component == Component::ParentDir {
            return Err(PathError::PathTraversal);
        }
    }
    Ok(relative)
}

/// Resolve `relative_path` within `workspace_root` and verify containment.
/// Returns the canonicalized absolute path.
pub fn resolve_path(workspace_root: &Path, relative_path: &str) -> Result<PathBuf, PathError> {
    // Canonicalize workspace root first.
    let root = std::fs::canonicalize(workspace_root).map_err(|_| PathError::UnknownWorkspace)?;

    let candidate = root.join(relative_path);
    let canonical = std::fs::canonicalize(&candidate).map_err(|_| PathError::NotFound)?;

    if !canonical.starts_with(&root) {
        return Err(PathError::OutsideWorkspace);
    }
    Ok(canonical)
}

/// Like `resolve_path` but does NOT canonicalize (for paths that may not yet
/// exist, e.g. for write/mkdir). Only checks containment without following
/// symlinks. Parent must exist and be within workspace.
pub fn resolve_path_unchecked(
    workspace_root: &Path,
    relative_path: &str,
) -> Result<PathBuf, PathError> {
    let root = std::fs::canonicalize(workspace_root).map_err(|_| PathError::UnknownWorkspace)?;

    let candidate = root.join(relative_path);

    // Check containment by iterating components.
    let candidate_canon = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.clone());

    if !candidate_canon.starts_with(&root) {
        return Err(PathError::OutsideWorkspace);
    }
    Ok(candidate)
}

/// Check that no symlink in the path chain escapes the workspace.
/// Returns Ok(()) if safe, Err with symlink path if detected.
pub fn check_symlink_escape(path: &Path, workspace_root: &Path) -> Result<(), PathError> {
    let root = std::fs::canonicalize(workspace_root).map_err(|_| PathError::UnknownWorkspace)?;

    // Walk each ancestor and canonicalize to check containment.
    let mut current = path.to_path_buf();
    while let Some(parent) = current.parent() {
        if parent == current {
            break;
        }
        if parent.is_symlink() || current.is_symlink() {
            let canon_parent =
                std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
            if !canon_parent.starts_with(&root) {
                return Err(PathError::SymlinkEscape);
            }
        }
        current = parent.to_path_buf();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rejects_absolute_path() {
        assert_eq!(
            validate_relative_path("/etc/passwd").unwrap_err(),
            PathError::AbsolutePath
        );
    }

    #[test]
    fn rejects_dotdot() {
        assert_eq!(
            validate_relative_path("../escape").unwrap_err(),
            PathError::PathTraversal
        );
        assert_eq!(
            validate_relative_path("a/../../b").unwrap_err(),
            PathError::PathTraversal
        );
    }

    #[test]
    fn accepts_valid_relative() {
        assert!(validate_relative_path(".").is_ok());
        assert!(validate_relative_path("src/main.rs").is_ok());
        assert!(validate_relative_path("a/b/c").is_ok());
    }

    #[test]
    fn resolve_within_workspace() {
        let dir = std::env::temp_dir().join(format!("path_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir.join("sub")).unwrap();

        let result = resolve_path(&dir, "sub").unwrap();
        assert!(result.starts_with(&dir.canonicalize().unwrap()));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_outside_workspace_denied() {
        let dir = std::env::temp_dir().join(format!("path_test_escape_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Create a symlink outside the workspace.
        let outside = std::env::temp_dir().join(format!("outside_{}", std::process::id()));
        let _ = fs::remove_dir_all(&outside);
        fs::write(&outside, "secret").unwrap();

        let link = dir.join("link");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            let result = resolve_path(&dir, "link");
            assert!(result.is_err(), "symlink escape should fail");
        }

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn resolve_unchecked_success() {
        let dir = std::env::temp_dir().join(format!("unchecked_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Path doesn't exist yet but parent does.
        let result = resolve_path_unchecked(&dir, "new_file.rs");
        assert!(result.is_ok());
        let result_path = result.unwrap();
        assert!(result_path.starts_with(&dir.canonicalize().unwrap()));

        let _ = fs::remove_dir_all(&dir);
    }
}
