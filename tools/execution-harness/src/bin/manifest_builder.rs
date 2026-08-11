use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let roots: Vec<String> = args
        .iter()
        .skip(1)
        .take_while(|arg| !arg.starts_with("--"))
        .cloned()
        .collect();
    if roots.is_empty() || roots.iter().any(|root| !root.starts_with('/')) {
        eprintln!("usage: manifest_builder /absolute/root [...] --endpoint URL --artifact-digest sha256:...");
        std::process::exit(2);
    }
    let endpoint =
        value_after(&args, "--endpoint").unwrap_or_else(|| "http://127.0.0.1:7650/execute".into());
    let artifact_digest = value_after(&args, "--artifact-digest").unwrap_or_else(|| {
        "sha256:0000000000000000000000000000000000000000000000000000000000000000".into()
    });
    for manifest in
        execution_harness::operation_specs::build_manifests(&roots, &endpoint, &artifact_digest)
    {
        let _ = writeln!(
            std::io::stderr(),
            "{}: {}",
            manifest.operation_name,
            manifest.manifest_id
        );
        println!(
            "{}",
            serde_json::to_string(&manifest).expect("serialize manifest")
        );
    }
}

fn value_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}
