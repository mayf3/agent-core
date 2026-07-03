//! Protocol dispatch — match operation name to handler.
//!
//! All handler logic is delegated to the library module
//! `agent_core_kernel::harness::coding::*`.

use agent_core_kernel::harness::coding::config::CodingConfig;
use agent_core_kernel::harness::coding::workspace;
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

    // Look up workspace root and permissions (except for task ops).
    let root = if is_task_op {
        None
    } else {
        let id = ws_id.as_ref().unwrap();
        match config.root_for(id) {
            Some(r) => {
                let perm = config.perm_for(id).unwrap();
                let needs_exec = operation == "external.coding_workspace_exec"
                    || operation == "external.coding_task_submit";
                let needs_zcode = operation == "external.coding_task_submit";
                let needs_write = operation == "external.coding_workspace_write";
                if needs_exec && !perm.exec {
                    return err_value("exec_not_permitted");
                }
                if needs_zcode {
                    // Check if the backend is "zcode" and if so require zcode permission.
                    if let Some(backend) = args.get("backend").and_then(Value::as_str) {
                        if backend == "zcode" && !perm.zcode {
                            return err_value("zcode_not_permitted");
                        }
                    }
                }
                if needs_write && !perm.write {
                    return err_value("write_not_permitted");
                }
                if !needs_exec && !needs_write && !is_task_op && !perm.read {
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
            workspace::handle_list(r, args)
        }
        "external.coding_workspace_read" => {
            let r = root.as_ref().unwrap();
            workspace::handle_read(r, args)
        }
        "external.coding_workspace_write" => {
            let r = root.as_ref().unwrap();
            workspace::handle_write(r, args)
        }
        "external.coding_workspace_exec" => {
            let r = root.as_ref().unwrap();
            let perm = config.perm_for(ws_id.as_ref().unwrap()).unwrap();
            workspace::handle_exec(r, args, perm)
        }
        "external.coding_task_submit" => {
            let ws = ws_id.as_ref().unwrap();
            let objective = args.get("objective").and_then(Value::as_str).unwrap_or("");
            let acceptance = args
                .get("acceptance_criteria")
                .and_then(Value::as_str)
                .unwrap_or("");
            let backend = args
                .get("backend")
                .and_then(Value::as_str)
                .unwrap_or("fake");
            agent_core_kernel::harness::coding::tasks::submit_task(
                ws, objective, acceptance, backend,
            )
        }
        "external.coding_task_status" => {
            let task_id = args.get("task_id").and_then(Value::as_str).unwrap_or("");
            agent_core_kernel::harness::coding::tasks::get_status(task_id)
        }
        "external.coding_capability_propose" => {
            // The propose handler needs journal, gateway, content_store, and agent_id.
            // These are not available from the standalone binary config, so we return
            // an error in the standalone example. Tests use the library directly.
            err_value("propose_not_available_in_standalone_example")
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
