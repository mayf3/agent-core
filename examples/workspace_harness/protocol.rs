//! Protocol dispatch — match operation name to handler and produce response JSON.

use crate::config::WorkspaceConfig;
use serde_json::Value;

/// Dispatch a harness operation to the appropriate handler.
/// Returns the JSON body for the HTTP response.
pub fn dispatch(config: &WorkspaceConfig, operation: &str, args: &Value) -> Value {
    // Determine workspace root from args.
    let root = {
        let workspace_id = match args.get("workspace_id").and_then(Value::as_str) {
            Some(id) => id,
            None => {
                return err_value("missing_workspace_id");
            }
        };
        match config.root_for(workspace_id) {
            Some(r) => r.clone(),
            None => {
                return err_value("unknown_workspace_id");
            }
        }
    };

    match operation {
        "external.workspace_list" => crate::fs_ops::handle_list(&root, args),
        "external.workspace_read" => crate::fs_ops::handle_read(&root, args),
        "external.workspace_write" => crate::fs_ops::handle_write(&root, args),
        "external.workspace_mkdir" => crate::fs_ops::handle_mkdir(&root, args),
        "external.workspace_stat" => crate::fs_ops::handle_stat(&root, args),
        "external.workspace_exec" => crate::exec::handle_exec(&root, args, &config.exec_env_pass),
        _ => serde_json::json!({
            "protocol_version": "external-harness-v1",
            "ok": false,
            "error_code": "unknown_operation",
        }),
    }
}

fn err_value(error_code: &str) -> Value {
    serde_json::json!({
        "protocol_version": "external-harness-v1",
        "ok": false,
        "error_code": error_code,
    })
}
