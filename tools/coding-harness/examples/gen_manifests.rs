//! Generate 7 canonical manifests as JSON for proposal submission.
use coding_harness::operation_specs;
use std::io::Write;

fn main() {
    let workspace_ids = vec!["agent-dev".to_string()];
    let endpoint = "http://127.0.0.1:7200";
    let artifact_digest = "sha256:14860707dc67499bc54b462c30331597b428c25b886e18cc246315feede344bd";
    let manifests = operation_specs::build_manifests(&workspace_ids, endpoint, artifact_digest);

    let out = std::env::temp_dir().join("coding-manifests");
    std::fs::create_dir_all(&out).unwrap();

    for m in &manifests {
        let path = out.join(format!("{}.json", m.operation_name));
        let mut f = std::fs::File::create(&path).unwrap();
        let manifest_json = serde_json::to_string_pretty(&serde_json::json!({
            "manifest_id": m.manifest_id,
            "harness_id": m.harness_id,
            "protocol_version": m.protocol_version,
            "endpoint": m.endpoint,
            "operation_name": m.operation_name,
            "description": m.description,
            "input_schema": m.input_schema,
            "output_schema": m.output_schema,
            "idempotent": m.idempotent,
            "artifact_digest": m.artifact_digest,
            "created_at": m.created_at.to_rfc3339(),
        }))
        .unwrap();
        f.write_all(manifest_json.as_bytes()).unwrap();
        eprintln!("Wrote {} -> {}", m.operation_name, path.display());
    }

    // Print summary as JSON for the parent script.
    let summary: Vec<serde_json::Value> = manifests
        .iter()
        .map(|m| {
            serde_json::json!({
                "operation_name": m.operation_name,
                "manifest_id": m.manifest_id,
                "endpoint": m.endpoint,
            })
        })
        .collect();
    println!("{}", serde_json::to_string(&summary).unwrap());
}
