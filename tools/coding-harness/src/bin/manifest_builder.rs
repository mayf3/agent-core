/// Canonical coding-harness manifest builder.
///
/// Reads the supported Coding operation specs, constructs real
/// `HarnessManifest` values via `build_manifests()`, computes each
/// `manifest_id` with `compute_manifest_id()`, and outputs canonical
/// manifest JSON to stdout.
///
/// This is the **only** canonical builder. No caller-supplied manifest_id
/// is accepted — the ID is always derived from the content.
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ws_ids: Vec<String> = args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .map(|a| a.to_string())
        .collect();
    let ws_ids = if ws_ids.is_empty() {
        vec!["default-workspace".to_string()]
    } else {
        ws_ids
    };

    let endpoint = args
        .iter()
        .position(|a| a == "--endpoint")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("http://127.0.0.1:7200/execute");
    let artifact_digest = args
        .iter()
        .position(|a| a == "--artifact-digest")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("sha256:0000000000000000000000000000000000000000000000000000000000000000");

    let manifests =
        coding_harness::operation_specs::build_manifests(&ws_ids, endpoint, artifact_digest);

    for m in &manifests {
        let json = serde_json::to_string_pretty(m).expect("serialize manifest");
        let _ = writeln!(
            std::io::stderr(),
            "{}: {}",
            m.operation_name, m.manifest_id
        );
        println!("{}", json);
    }
}
