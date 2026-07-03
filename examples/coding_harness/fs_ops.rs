//! File-system operations: list, read, write.

use crate::paths::{resolve_path, resolve_path_unchecked, validate_relative};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_LIST: usize = 200;
const MAX_WRITE: usize = 2 * 1024 * 1024;
const MAX_READ: usize = 65536;
static SEQ: AtomicU64 = AtomicU64::new(0);

fn ok_v(r: Value) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":true,"result":r})
}
fn err_v(c: &str) -> Value {
    json!({"protocol_version":"external-harness-v1","ok":false,"error_code":c})
}

pub fn handle_list(root: &Path, args: &Value) -> Value {
    let relative = match validate_relative(
        args.get("relative_path")
            .and_then(Value::as_str)
            .unwrap_or("."),
    ) {
        Ok(r) => r,
        Err(e) => return err_v(&e.to_string()),
    };
    let dir_path = match resolve_path(root, relative) {
        Ok(p) => p,
        Err(e) => return err_v(&e.to_string()),
    };
    if !dir_path.is_dir() {
        return err_v("not_a_directory");
    }
    let mut entries = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir_path) {
        for entry in rd.flatten().take(MAX_LIST) {
            let name = entry.file_name().to_string_lossy().to_string();
            let ft = entry.file_type().ok();
            let typ = if ft.map(|t| t.is_dir()).unwrap_or(false) {
                "dir"
            } else {
                "file"
            };
            let ep = entry.path();
            let rp = ep
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            entries.push(json!({"name": name, "type": typ, "relative_path": rp}));
        }
    }
    ok_v(json!({"entries": entries, "entry_count": entries.len()}))
}

pub fn handle_read(root: &Path, args: &Value) -> Value {
    let relative = match validate_relative(
        args.get("relative_path")
            .and_then(Value::as_str)
            .unwrap_or(""),
    ) {
        Ok(r) => r,
        Err(e) => return err_v(&e.to_string()),
    };
    let path = match resolve_path(root, relative) {
        Ok(p) => p,
        Err(e) => return err_v(&e.to_string()),
    };
    if !path.is_file() {
        return err_v("not_a_file");
    }
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(65536)
        .min(MAX_READ as u64) as usize;
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => return err_v(&format!("read_failed: {e}")),
    };
    let slice = &data[..data.len().min(max_bytes)];
    let content = match String::from_utf8(slice.to_vec()) {
        Ok(s) => s,
        Err(_) => return err_v("binary_file_not_supported"),
    };
    ok_v(
        json!({"content": content, "truncated": data.len() > max_bytes, "size_bytes": data.len(), "bytes_read": content.len()}),
    )
}

pub fn handle_write(root: &Path, args: &Value) -> Value {
    let relative = match validate_relative(
        args.get("relative_path")
            .and_then(Value::as_str)
            .unwrap_or(""),
    ) {
        Ok(r) => r,
        Err(e) => return err_v(&e.to_string()),
    };
    let content = args.get("content").and_then(Value::as_str).unwrap_or("");
    let mode = args
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("replace");
    if content.len() > MAX_WRITE {
        return err_v("content_exceeds_max_size");
    }

    let path = match resolve_path_unchecked(root, relative) {
        Ok(p) => p,
        Err(e) => return err_v(&e.to_string()),
    };
    if path.exists() && path.is_dir() {
        return err_v("is_a_directory");
    }
    if path.exists() && path.is_symlink() {
        return err_v("symlink_write_not_allowed");
    }

    if let Some(parent) = path.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return err_v(&format!("mkdir_failed: {e}"));
            }
        }
    }

    let bytes = match mode {
        "replace" => content.as_bytes().to_vec(),
        "append" => {
            let mut e = std::fs::read(&path).unwrap_or_default();
            e.extend_from_slice(content.as_bytes());
            e
        }
        other => return err_v(&format!("unknown_mode: {other}")),
    };

    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!(".tmp_{}_{}", std::process::id(), seq));
    let wr = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = wr {
        let _ = std::fs::remove_file(&tmp);
        return err_v(&format!("write_failed: {e}"));
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return err_v(&format!("rename_failed: {e}"));
    }

    let mut h = Sha256::new();
    h.update(&bytes);
    ok_v(json!({"bytes_written": bytes.len(), "sha256": hex::encode(h.finalize()), "mode": mode}))
}
