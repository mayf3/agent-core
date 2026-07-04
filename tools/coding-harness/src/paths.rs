use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    UnknownWorkspace,
    AbsolutePath,
    PathTraversal,
    OutsideWorkspace,
    SymlinkEscape,
    NotFound,
    PermissionDenied,
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
            PathError::PermissionDenied => write!(f, "permission_denied"),
        }
    }
}

/// Validate that a relative path is safe (no absolute, no ..).
pub fn validate_relative(relative: &str) -> Result<&str, PathError> {
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

/// Resolve a relative path within root, following symlinks (canonicalize).
pub fn resolve_path(root: &Path, relative: &str) -> Result<PathBuf, PathError> {
    let root_canon = std::fs::canonicalize(root).map_err(|_| PathError::UnknownWorkspace)?;
    let candidate = root_canon.join(relative);
    let canonical = std::fs::canonicalize(&candidate).map_err(|_| PathError::NotFound)?;
    if !canonical.starts_with(&root_canon) {
        return Err(PathError::OutsideWorkspace);
    }
    Ok(canonical)
}

/// Resolve a relative path without following symlinks on the final component.
pub fn resolve_path_unchecked(root: &Path, relative: &str) -> Result<PathBuf, PathError> {
    let root_canon = std::fs::canonicalize(root).map_err(|_| PathError::UnknownWorkspace)?;
    let candidate = root_canon.join(relative);
    let nearest = nearest_existing(&candidate);
    let canon_parent = std::fs::canonicalize(&nearest).map_err(|_| PathError::NotFound)?;
    if !canon_parent.starts_with(&root_canon) {
        return Err(PathError::OutsideWorkspace);
    }
    Ok(candidate)
}

fn nearest_existing(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    loop {
        if cur.exists() {
            return cur;
        }
        match cur.parent() {
            Some(p) if p != cur => cur = p.to_path_buf(),
            _ => return cur,
        }
    }
}

/// Check that a file path doesn't escape workspace via symlinks.
pub fn assert_no_symlink_escape(root: &Path, path: &Path) -> Result<(), PathError> {
    let root_canon = std::fs::canonicalize(root).map_err(|_| PathError::UnknownWorkspace)?;
    if path.is_symlink() {
        let target = std::fs::read_link(path).map_err(|_| PathError::SymlinkEscape)?;
        if !target.is_absolute() {
            let resolved = path.parent().unwrap_or(root).join(&target);
            let canon = std::fs::canonicalize(&resolved).map_err(|_| PathError::SymlinkEscape)?;
            if !canon.starts_with(&root_canon) {
                return Err(PathError::SymlinkEscape);
            }
        } else if !target.starts_with(&root_canon) {
            return Err(PathError::SymlinkEscape);
        }
    }
    Ok(())
}
