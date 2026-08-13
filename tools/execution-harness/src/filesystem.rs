//! Filesystem primitives over real absolute paths with an allowed-root fence.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const DEFAULT_MAX_READ: usize = 64 * 1024;
const HARD_MAX_READ: usize = 64 * 1024;
const MAX_WRITE: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct AllowedRoots {
    roots: Vec<PathBuf>,
}

impl AllowedRoots {
    pub fn new(roots: Vec<PathBuf>) -> Result<Self, String> {
        if roots.is_empty() {
            return Err("allowed_roots_empty".into());
        }
        let mut canonical = Vec::with_capacity(roots.len());
        for root in roots {
            if !root.is_absolute() {
                return Err("allowed_root_not_absolute".into());
            }
            let root = root
                .canonicalize()
                .map_err(|_| "allowed_root_not_found".to_string())?;
            if !root.is_dir() {
                return Err("allowed_root_not_directory".into());
            }
            if !canonical.contains(&root) {
                canonical.push(root);
            }
        }
        Ok(Self { roots: canonical })
    }

    pub fn as_slice(&self) -> &[PathBuf] {
        &self.roots
    }

    fn is_allowed(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| path.starts_with(root))
    }

    pub fn resolve_existing(&self, raw: &str) -> Result<PathBuf, String> {
        let path = required_absolute_path(raw)?;
        let canonical = path
            .canonicalize()
            .map_err(|_| "path_not_found".to_string())?;
        if !self.is_allowed(&canonical) {
            return Err("path_escape".into());
        }
        Ok(canonical)
    }

    pub fn resolve_write(&self, raw: &str) -> Result<PathBuf, String> {
        let path = required_absolute_path(raw)?;
        if path.exists() {
            let canonical = path
                .canonicalize()
                .map_err(|_| "path_not_found".to_string())?;
            if !self.is_allowed(&canonical) {
                return Err("path_escape".into());
            }
            if !canonical.is_file() {
                return Err("path_not_file".into());
            }
            return Ok(canonical);
        }

        let parent = path
            .parent()
            .ok_or_else(|| "parent_not_found".to_string())?;
        let parent = parent
            .canonicalize()
            .map_err(|_| "parent_not_found".to_string())?;
        if !self.is_allowed(&parent) {
            return Err("path_escape".into());
        }
        if !parent.is_dir() {
            return Err("parent_not_directory".into());
        }
        let name = path
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| "path_not_file".to_string())?;
        Ok(parent.join(name))
    }
}

fn required_absolute_path(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("missing_path".into());
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err("path_not_absolute".into());
    }
    Ok(path)
}

fn argument_path(arguments: &Value) -> Result<&str, String> {
    arguments
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| "missing_path".to_string())
}

pub fn list(roots: &AllowedRoots, arguments: &Value) -> Result<Value, String> {
    let path = roots.resolve_existing(argument_path(arguments)?)?;
    if !path.is_dir() {
        return Err("path_not_directory".into());
    }
    let entries = std::fs::read_dir(&path).map_err(|_| "path_unreadable".to_string())?;
    let mut rows: Vec<Value> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            let entry_path = entry.path();
            let file_type = entry.file_type().ok();
            let kind = match file_type {
                Some(t) if t.is_dir() => "directory",
                Some(t) if t.is_symlink() => "symlink",
                _ => "file",
            };
            json!({
                "name": entry.file_name().to_string_lossy(),
                "path": entry_path.to_string_lossy(),
                "type": kind,
            })
        })
        .collect();
    rows.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(json!({
        "path": path.to_string_lossy(),
        "entries": rows,
        "entry_count": rows.len(),
    }))
}

pub fn read(roots: &AllowedRoots, arguments: &Value) -> Result<Value, String> {
    let path = roots.resolve_existing(argument_path(arguments)?)?;
    if !path.is_file() {
        return Err("file_not_found".into());
    }
    let max_bytes = arguments
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_MAX_READ as u64)
        .clamp(1, HARD_MAX_READ as u64) as usize;
    let bytes = std::fs::read(&path).map_err(|_| "file_unreadable".to_string())?;
    let size = bytes.len();
    let take = size.min(max_bytes);
    let content = std::str::from_utf8(&bytes[..take])
        .map_err(|_| "file_not_utf8".to_string())?
        .to_string();
    Ok(json!({
        "path": path.to_string_lossy(),
        "content": content,
        "size_bytes": size,
        "bytes_read": take,
        "truncated": take < size,
    }))
}

pub fn write(roots: &AllowedRoots, arguments: &Value) -> Result<Value, String> {
    let path = roots.resolve_write(argument_path(arguments)?)?;
    let content = arguments
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing_content".to_string())?;
    if content.len() > MAX_WRITE {
        return Err("content_too_large".into());
    }

    #[cfg(unix)]
    let options = {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = std::fs::OpenOptions::new();
        options
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW);
        options
    };
    #[cfg(not(unix))]
    let options = {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        options
    };

    use std::io::Write;
    let mut file = options
        .open(&path)
        .map_err(|_| "file_write_failed".to_string())?;
    file.write_all(content.as_bytes())
        .map_err(|_| "file_write_failed".to_string())?;
    file.sync_all()
        .map_err(|_| "file_write_failed".to_string())?;
    Ok(json!({
        "path": path.to_string_lossy(),
        "bytes_written": content.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "execution-harness-filesystem-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn list_read_and_write_use_absolute_paths() {
        let root = temp_root("roundtrip");
        let roots = AllowedRoots::new(vec![root.clone()]).unwrap();
        let file = root.join("note.txt");
        let written = write(&roots, &json!({"path": file, "content": "hello"})).unwrap();
        assert_eq!(written["bytes_written"], 5);
        let read_back = read(&roots, &json!({"path": file})).unwrap();
        assert_eq!(read_back["content"], "hello");
        let listing = list(&roots, &json!({"path": root})).unwrap();
        assert_eq!(listing["entry_count"], 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_parent_is_not_created() {
        let root = temp_root("missing-parent");
        let roots = AllowedRoots::new(vec![root.clone()]).unwrap();
        let missing = root.join("unknown").join("file.txt");
        assert_eq!(
            write(&roots, &json!({"path": missing, "content": "x"})).unwrap_err(),
            "parent_not_found"
        );
        assert!(!root.join("unknown").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_path_is_reported_without_creating_it() {
        let root = temp_root("missing-path");
        let roots = AllowedRoots::new(vec![root.clone()]).unwrap();
        let missing = root.join("missing.txt");
        assert_eq!(
            read(&roots, &json!({"path": missing})).unwrap_err(),
            "path_not_found"
        );
        assert!(!root.join("missing.txt").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn traversal_and_outside_absolute_paths_are_rejected() {
        let root = temp_root("escape");
        let outside = temp_root("outside");
        let roots = AllowedRoots::new(vec![root.clone()]).unwrap();
        assert_eq!(
            read(&roots, &json!({"path": outside.join("missing")})).unwrap_err(),
            "path_not_found"
        );
        std::fs::write(outside.join("secret"), "no").unwrap();
        assert_eq!(
            read(&roots, &json!({"path": outside.join("secret")})).unwrap_err(),
            "path_escape"
        );
        assert_eq!(
            write(
                &roots,
                &json!({"path": root.join("..").join(outside.file_name().unwrap()).join("x"), "content": "x"})
            )
            .unwrap_err(),
            "path_escape"
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected_for_read_and_write() {
        use std::os::unix::fs::symlink;
        let root = temp_root("symlink");
        let outside = temp_root("symlink-outside");
        std::fs::write(outside.join("secret"), "no").unwrap();
        symlink(&outside, root.join("link")).unwrap();
        let roots = AllowedRoots::new(vec![root.clone()]).unwrap();
        assert_eq!(
            read(&roots, &json!({"path": root.join("link/secret")})).unwrap_err(),
            "path_escape"
        );
        assert_eq!(
            write(
                &roots,
                &json!({"path": root.join("link/new"), "content": "x"})
            )
            .unwrap_err(),
            "path_escape"
        );
        assert!(!outside.join("new").exists());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }
}
