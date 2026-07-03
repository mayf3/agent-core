//! Protocol dispatch — match operation name to handler.

use crate::config::CodingConfig;
use serde_json::Value;

pub fn dispatch(config: &CodingConfig, operation: &str, args: &Value) -> Value {
    // All operations need workspace_id (except task.status which uses task_id).
    let is_task_op = operation == "external.coding_task_status";

    let ws_id = if is_task_op {
        None // task ops don't need workspace_id
    } else {
        match args.get("workspace_id").and_then(Value::as_str) {
            Some(id) => Some(id.to_string()),
            None => return err_value("missing_workspace_id"),
        }
    };

    // Look up workspace root (except for task ops).
    let root = if is_task_op {
        None
    } else {
        let id = ws_id.as_ref().unwrap();
        match config.root_for(id) {
            Some(r) => {
                // Check basic permission: read/write ops need read, exec needs exec.
                let perm = config.perm_for(id).unwrap();
                let needs_exec = operation == "external.coding_workspace_exec"
                    || operation == "external.coding_task_submit";
                let needs_write = operation == "external.coding_workspace_write";
                if needs_exec && !perm.exec {
                    return err_value("exec_not_permitted");
                }
                if needs_write && !perm.write {
                    return err_value("write_not_permitted");
                }
                if !needs_exec && !needs_write && !perm.read {
                    return err_value("read_not_permitted");
                }
                Some(r.clone())
            }
            None => return err_value("unknown_workspace_id"),
        }
    };

    match operation {
        "external.coding_workspace_list" => {
            let r = root.as_ref().unwrap();
            crate::fs_ops::handle_list(r, args)
        }
        "external.coding_workspace_read" => {
            let r = root.as_ref().unwrap();
            crate::fs_ops::handle_read(r, args)
        }
        "external.coding_workspace_write" => {
            let r = root.as_ref().unwrap();
            crate::fs_ops::handle_write(r, args)
        }
        "external.coding_workspace_exec" => {
            let r = root.as_ref().unwrap();
            crate::exec::handle_exec(r, args)
        }
        "external.coding_task_submit" => {
            let ws = ws_id.as_ref().unwrap();
            let objective = args.get("objective").and_then(Value::as_str).unwrap_or("");
            let backend = args
                .get("backend")
                .and_then(Value::as_str)
                .unwrap_or("zcode");
            crate::tasks::submit_task(ws, objective, backend)
        }
        "external.coding_task_status" => {
            let task_id = args.get("task_id").and_then(Value::as_str).unwrap_or("");
            crate::tasks::get_status(task_id)
        }
        "external.coding_capability_propose" => {
            crate::capability::handle_propose(args, root.as_ref())
        }
        _ => err_value("unknown_operation"),
    }
}

fn err_value(code: &str) -> Value {
    serde_json::json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "error_code": code,
    })
}
