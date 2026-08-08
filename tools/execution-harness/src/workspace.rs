//! Workspace file primitives with strict path fencing.
//!
//! Every workspace is a subdirectory of the harness workspace root, named
//! by `workspace_id` (restricted to `[A-Za-z0-9_-]+`). All reads/writes are
//! canonicalized and must resolve inside the workspace directory — `..`
//! escapes, absolute paths and symlink escapes are rejected.

use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

/// Validate a workspace_id component (no separators, no dots, no traversal).
fn validate_workspace_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("missing_workspace_id".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("invalid_workspace_id".into());
    }
    Ok(())
}

/// Resolve `workspace_id` to its canonical directory under `root`, creating
/// it if missing. Fails if the resolved path escapes the root.
fn workspace_dir(root: &Path, workspace_id: &str) -> Result<PathBuf, String> {
    validate_workspace_id(workspace_id)?;
    let dir = root.join(workspace_id);
    std::fs::create_dir_all(&dir).map_err(|_| "workspace_create_failed".to_string())?;
    let canonical = dir.canonicalize().map_err(|_| "workspace_unreadable".to_string())?;
    if !canonical.starts_with(root) {
        return Err("path_escape".into());
    }
    Ok(canonical)
}

/// Resolve a relative path inside `workspace`, rejecting any traversal or
/// absolute path. Returns the canonical path if it exists (or its parent's
/// canonical path plus the final component for writes).
fn resolve_fenced(root: &Path, workspace_id: &str, relative_path: &str) -> Result<PathBuf, String> {
    if relative_path.is_empty() {
        return Err("missing_relative_path".into());
    }
    let rel = Path::new(relative_path);
    if rel.is_absolute() {
        return Err("path_escape".into());
    }
    // Reject any .. component or non-normal component.
    for comp in rel.components() {
        match comp {
            Component::Normal(_) => {}
            Component::CurDir => {}
            _ => return Err("path_escape".into()),
        }
    }
    let ws = workspace_dir(root, workspace_id)?;
    let joined = ws.join(rel);
    let canonical = joined.canonicalize().unwrap_or(joined);
    if !canonical.starts_with(&ws) {
        return Err("path_escape".into());
    }
    Ok(canonical)
}

pub fn list(root: &Path, arguments: &Value) -> Result<Value, String> {
    let workspace_id = arguments
        .get("workspace_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing_workspace_id".to_string())?;
    let ws = workspace_dir(root, workspace_id)?;
    let entries = std::fs::read_dir(&ws).map_err(|_| "workspace_unreadable".to_string())?;
    let mut rows: Vec<Value> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let ftype = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            json!({
                "name": name,
                "relative_path": name,
                "type": if ftype { "directory" } else { "file" },
            })
        })
        .collect();
    rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(json!({
        "entries": rows,
        "entry_count": rows.len(),
        "workspace_id": workspace_id,
    }))
}

pub fn read(root: &Path, arguments: &Value) -> Result<Value, String> {
    let workspace_id = arguments
        .get("workspace_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing_workspace_id".to_string())?;
    let relative_path = arguments
        .get("relative_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing_relative_path".to_string())?;
    let path = resolve_fenced(root, workspace_id, relative_path)?;
    if !path.is_file() {
        return Err("file_not_found".into());
    }
    let content = std::fs::read_to_string(&path).map_err(|_| "file_unreadable".to_string())?;
    Ok(json!({
        "workspace_id": workspace_id,
        "relative_path": relative_path,
        "content": content,
        "bytes": content.len(),
    }))
}

pub fn write(root: &Path, arguments: &Value) -> Result<Value, String> {
    let workspace_id = arguments
        .get("workspace_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing_workspace_id".to_string())?;
    let relative_path = arguments
        .get("relative_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing_relative_path".to_string())?;
    let content = arguments
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing_content".to_string())?;
    // Cap write size (keep responses and disks sane).
    if content.len() > 4 * 1024 * 1024 {
        return Err("content_too_large".into());
    }
    let path = resolve_fenced(root, workspace_id, relative_path)?;
    // Create parent directories within the fence.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| "workspace_create_failed".to_string())?;
    }
    std::fs::write(&path, content).map_err(|_| "file_write_failed".to_string())?;
    Ok(json!({
        "workspace_id": workspace_id,
        "relative_path": relative_path,
        "ok": true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "exec-harness-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn write_read_roundtrip_and_list() {
        let root = tmp_root("wrl");
        let w = write(
            &root,
            &json!({"workspace_id": "ws1", "relative_path": "src/main.rs", "content": "fn main() {}"}),
        )
        .unwrap();
        assert_eq!(w["ok"], true);
        let r = read(
            &root,
            &json!({"workspace_id": "ws1", "relative_path": "src/main.rs"}),
        )
        .unwrap();
        assert_eq!(r["content"], "fn main() {}");
        let l = list(&root, &json!({"workspace_id": "ws1"})).unwrap();
        assert_eq!(l["entry_count"], 1);
        assert_eq!(l["entries"][0]["relative_path"], "src");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn traversal_escapes_are_rejected() {
        let root = tmp_root("esc");
        for bad in ["../evil", "a/../../evil", "/etc/passwd", ".."] {
            let r = write(
                &root,
                &json!({"workspace_id": "ws2", "relative_path": bad, "content": "x"}),
            );
            assert!(r.is_err(), "{bad} must be rejected");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_workspace_id_rejected() {
        let root = tmp_root("wsid");
        for bad in ["", "a/b", "..", "a b"] {
            let r = list(&root, &json!({"workspace_id": bad}));
            assert!(r.is_err(), "{bad:?} must be rejected");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
