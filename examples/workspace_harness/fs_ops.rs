//! File-system operations: list, read, write, mkdir, stat.
//!
//! Every handler takes the workspace root and parsed arguments, returns a
//! JSON response envelope that matches `external-harness-v1` protocol.

use crate::paths::{resolve_path, resolve_path_unchecked, validate_relative_path};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

const MAX_LIST_ENTRIES: usize = 200;
const MAX_WRITE_BYTES: usize = 2 * 1024 * 1024; // 2 MiB
const MAX_READ_BYTES: usize = 65536;

pub fn handle_list(root: &Path, args: &Value) -> Value {
    let workspace_id = match args.get("workspace_id").and_then(Value::as_str) {
        Some(id) => id,
        None => return err("missing_workspace_id"),
    };
    // workspace_id is validated by caller-passed root; we trust root is correct.
    let _ = workspace_id;

    let relative = args
        .get("relative_path")
        .and_then(Value::as_str)
        .unwrap_or(".");
    let max_entries = args
        .get("max_entries")
        .and_then(Value::as_u64)
        .unwrap_or(200)
        .min(MAX_LIST_ENTRIES as u64) as usize;
    let max_depth: usize = args
        .get("recursive_depth")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    if max_depth > 3 {
        return err("recursive_depth_too_deep");
    }

    let relative = match validate_relative_path(relative) {
        Ok(r) => r,
        Err(e) => return err(&e.to_string()),
    };

    let dir_path = match resolve_path(root, relative) {
        Ok(p) => p,
        Err(e) => return err(&e.to_string()),
    };

    if !dir_path.is_dir() {
        return err("not_a_directory");
    }

    let mut entries = Vec::new();
    if max_depth == 0 {
        let dir_iter = match std::fs::read_dir(&dir_path) {
            Ok(it) => it,
            Err(e) => return err(&format!("read_dir_failed: {e}")),
        };
        for entry in dir_iter.flatten().take(max_entries) {
            let name = entry.file_name().to_string_lossy().to_string();
            let ft = entry.file_type().ok();
            let entry_type = if ft.map(|t| t.is_dir()).unwrap_or(false) {
                "dir"
            } else if ft.map(|t| t.is_symlink()).unwrap_or(false) {
                "symlink"
            } else {
                "file"
            };
            let entry_path = entry.path();
            let rel = entry_path.strip_prefix(root).ok();
            let relative_path = rel
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            entries.push(json!({
                "name": name,
                "type": entry_type,
                "relative_path": relative_path,
            }));
        }
    } else {
        collect_entries(root, &dir_path, "", max_depth, &mut entries, max_entries);
    }

    ok(json!({
        "entries": entries,
        "entry_count": entries.len(),
    }))
}

fn collect_entries(
    root: &Path,
    dir: &Path,
    prefix: &str,
    remaining_depth: usize,
    entries: &mut Vec<Value>,
    max: usize,
) {
    if entries.len() >= max {
        return;
    }
    let dir_iter = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in dir_iter.flatten() {
        if entries.len() >= max {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let ft = entry.file_type().ok();
        let is_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
        let is_symlink = ft.map(|t| t.is_symlink()).unwrap_or(false);
        let entry_type = if is_dir {
            "dir"
        } else if is_symlink {
            "symlink"
        } else {
            "file"
        };
        let entry_path = entry.path();
        let rel = entry_path.strip_prefix(root).ok();
        let relative_path = rel
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let full_prefix = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        entries.push(json!({
            "name": full_prefix,
            "type": entry_type,
            "relative_path": relative_path,
        }));
        if is_dir && remaining_depth > 0 {
            collect_entries(
                root,
                &entry.path(),
                &full_prefix,
                remaining_depth - 1,
                entries,
                max,
            );
        }
    }
}

pub fn handle_read(root: &Path, args: &Value) -> Value {
    let relative = match args.get("relative_path").and_then(Value::as_str) {
        Some(r) => r,
        None => return err("missing_relative_path"),
    };
    match validate_relative_path(relative) {
        Ok(_) => {}
        Err(e) => return err(&e.to_string()),
    }

    let path = match resolve_path(root, relative) {
        Ok(p) => p,
        Err(e) => return err(&e.to_string()),
    };

    if !path.is_file() {
        return err("not_a_file");
    }

    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(65536)
        .min(MAX_READ_BYTES as u64) as usize;

    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;

    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => return err(&format!("read_failed: {e}")),
    };

    if offset > data.len() {
        return ok(json!({
            "content": "",
            "truncated": false,
            "size_bytes": data.len(),
            "offset": offset,
            "bytes_read": 0,
        }));
    }

    let slice = &data[offset..];
    let truncated = slice.len() > max_bytes;
    let take_len = std::cmp::min(slice.len(), max_bytes);
    let content_slice = &slice[..take_len];

    // Reject non-UTF-8 binary files.
    let content = match String::from_utf8(content_slice.to_vec()) {
        Ok(s) => s,
        Err(_) => return err("binary_file_not_supported"),
    };

    ok(json!({
        "content": content,
        "truncated": truncated,
        "size_bytes": data.len(),
        "offset": offset,
        "bytes_read": content.len(),
    }))
}

pub fn handle_write(root: &Path, args: &Value) -> Value {
    let relative = match args.get("relative_path").and_then(Value::as_str) {
        Some(r) => r,
        None => return err("missing_relative_path"),
    };
    let content = match args.get("content").and_then(Value::as_str) {
        Some(c) => c,
        None => return err("missing_content"),
    };
    let mode = args
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("replace");

    match validate_relative_path(relative) {
        Ok(_) => {}
        Err(e) => return err(&e.to_string()),
    }

    if content.len() > MAX_WRITE_BYTES {
        return err("content_exceeds_max_size");
    }

    // Use resolve_path_unchecked for paths that may not exist yet.
    let path = match resolve_path_unchecked(root, relative) {
        Ok(p) => p,
        Err(e) => return err(&e.to_string()),
    };

    // Reject if it's an existing directory.
    if path.exists() && path.is_dir() {
        return err("is_a_directory");
    }

    // Check parent exists.
    let parent = match path.parent() {
        Some(p) => p,
        None => return err("no_parent_directory"),
    };
    if !parent.exists() {
        return err("parent_directory_not_found");
    }

    // Check symlink escape.
    if path.exists() {
        if path.is_symlink() {
            return err("symlink_write_not_allowed");
        }
        if let Err(e) = crate::paths::check_symlink_escape(&path, root) {
            return err(&e.to_string());
        }
    }

    // Build the content to write.
    let bytes = match mode {
        "replace" => content.as_bytes().to_vec(),
        "append" => {
            let mut existing = match std::fs::read(&path) {
                Ok(d) => d,
                Err(_) => Vec::new(),
            };
            existing.extend_from_slice(content.as_bytes());
            existing
        }
        other => return err(&format!("unknown_mode: {other}")),
    };

    // Atomic write: temp file + rename.
    let tmp_path = path.with_extension(format!(".tmp_{}", std::process::id()));

    match std::fs::write(&tmp_path, &bytes) {
        Ok(_) => {}
        Err(e) => return err(&format!("write_failed: {e}")),
    }

    if let Err(e) = std::fs::rename(&tmp_path, &path) {
        let _ = std::fs::remove_file(&tmp_path);
        return err(&format!("rename_failed: {e}"));
    }

    // Compute SHA-256.
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sha256 = hex::encode(hasher.finalize());

    ok(json!({
        "bytes_written": bytes.len(),
        "sha256": sha256,
        "mode": mode,
    }))
}

pub fn handle_mkdir(root: &Path, args: &Value) -> Value {
    let relative = match args.get("relative_path").and_then(Value::as_str) {
        Some(r) => r,
        None => return err("missing_relative_path"),
    };
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    match validate_relative_path(relative) {
        Ok(_) => {}
        Err(e) => return err(&e.to_string()),
    }

    let path = match resolve_path_unchecked(root, relative) {
        Ok(p) => p,
        Err(e) => return err(&e.to_string()),
    };

    if path.exists() {
        return ok(json!({"created": false, "already_exists": true}));
    }

    if recursive {
        match std::fs::create_dir_all(&path) {
            Ok(_) => ok(json!({"created": true})),
            Err(e) => err(&format!("mkdir_failed: {e}")),
        }
    } else {
        match std::fs::create_dir(&path) {
            Ok(_) => ok(json!({"created": true})),
            Err(e) => err(&format!("mkdir_failed: {e}")),
        }
    }
}

pub fn handle_stat(root: &Path, args: &Value) -> Value {
    let relative = match args.get("relative_path").and_then(Value::as_str) {
        Some(r) => r,
        None => return err("missing_relative_path"),
    };

    match validate_relative_path(relative) {
        Ok(_) => {}
        Err(e) => return err(&e.to_string()),
    }

    let path = match resolve_path(root, relative) {
        Ok(p) => p,
        Err(e) => return err(&e.to_string()),
    };

    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) => return err(&format!("stat_failed: {e}")),
    };

    let is_symlink = metadata.file_type().is_symlink();

    // If it's a symlink, check it doesn't escape workspace.
    if is_symlink {
        if let Err(e) = crate::paths::check_symlink_escape(&path, root) {
            return err(&e.to_string());
        }
    }

    let entry_type = if metadata.file_type().is_dir() {
        "dir"
    } else if is_symlink {
        "symlink"
    } else {
        "file"
    };

    let modified_at = match metadata.modified() {
        Ok(time) => {
            let duration = time
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            duration.as_secs()
        }
        Err(_) => 0,
    };

    ok(json!({
        "type": entry_type,
        "size_bytes": metadata.len(),
        "modified_at_unix": modified_at,
        "is_symlink": is_symlink,
    }))
}

// ── Helpers ──

fn ok(result: Value) -> Value {
    json!({
        "protocol_version": "external-harness-v1",
        "ok": true,
        "result": result,
    })
}

fn err(code: &str) -> Value {
    json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "error_code": code,
    })
}
