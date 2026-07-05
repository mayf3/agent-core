use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::LocalEchoLlm;
use agent_core_kernel::runtime::Runtime;
use agent_core_kernel::server::serve;
use anyhow::{bail, Result};
use serde_json::json;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run_cli(&args[1..]),
        Some("serve") => serve_cli(&args[1..]),
        Some("check-coding-schema-integrity") => check_coding_schema_integrity(&args[1..]),
        _ => {
            print_help();
            Ok(())
        }
    }
}

/// Read-only integrity check for coding-harness registry state.
///
/// Connects to the kernel database and verifies:
/// - Coding manifest IDs are recomputable and match stored digests
/// - Input schemas are non-empty (contain workspace_id)
/// - No snapshot-level inconsistencies for coding operations
///
/// Never modifies the database.
fn check_coding_schema_integrity(args: &[String]) -> Result<()> {
    let opts = CliOptions::parse_for_check(args)?;
    let config = KernelConfig::from_cli(opts.db_path);
    use agent_core_kernel::domain::operation::external;
    use agent_core_kernel::journal::JournalStore;
    let journal = JournalStore::open(&config.db_path)?;
    let snapshot_id = journal.current_registry_snapshot_id()?;
    let snapshot = journal.load_registry_snapshot(&snapshot_id)?;
    let mut issues = Vec::new();

    for op_name in external::CODING_OPERATIONS {
        let spec = snapshot.lookup(op_name);
        match spec {
            None => issues.push(format!("{op_name}: not found in active snapshot")),
            Some(s) => {
                let has_ws = s
                    .parameters
                    .get("properties")
                    .and_then(|p| p.get("workspace_id"))
                    .is_some();
                if !has_ws {
                    issues.push(format!(
                        "{op_name}: input schema is empty (no workspace_id)"
                    ));
                }
                // Verify manifest ID consistency if the manifest exists.
                if s.binding_kind == agent_core_kernel::registry::snapshot::BindingKind::External {
                    match journal.load_harness_manifest(&s.binding_key) {
                        Ok(Some(m)) => {
                            let recomputed = m.compute_manifest_id()?;
                            if recomputed != m.manifest_id {
                                issues.push(format!(
                                    "{op_name}: manifest_id mismatch stored={} recomputed={}",
                                    m.manifest_id, recomputed
                                ));
                            }
                        }
                        Ok(None) => {
                            issues.push(format!("{op_name}: manifest {} not found", s.binding_key));
                        }
                        Err(e) => {
                            issues.push(format!("{op_name}: manifest load error: {e}"));
                        }
                    }
                }
            }
        }
    }

    if issues.is_empty() {
        eprintln!("All coding operations pass integrity check.");
        Ok(())
    } else {
        for issue in &issues {
            eprintln!("ISSUE: {issue}");
        }
        Err(anyhow::anyhow!("{} integrity issue(s) found", issues.len()))
    }
}

fn run_cli(args: &[String]) -> Result<()> {
    let options = CliOptions::parse(args)?;
    let config = KernelConfig::from_cli(options.db_path);
    let journal = JournalStore::open(&config.db_path)?;
    journal.initialize_registry()?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm);
    let envelope = gateway.cli_ingress(options.text)?;
    let validated = gateway.validate_ingress(&journal, envelope)?;
    let outcome = runtime.deliver(&journal, &gateway, validated)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "run_id": outcome.run_id.0,
                "session_id": outcome.session_id.0,
                "output": outcome.output,
            }))?
        );
    } else {
        println!("{}", outcome.output);
    }
    Ok(())
}

fn serve_cli(args: &[String]) -> Result<()> {
    let options = ServeOptions::parse(args)?;
    let mut config = KernelConfig::from_cli(options.db_path);
    if let Some(port) = options.port {
        config.kernel_port = port;
    }
    serve(config)
}

struct CliOptions {
    text: String,
    db_path: Option<String>,
    json: bool,
}

impl CliOptions {
    fn parse_for_check(args: &[String]) -> Result<Self> {
        let mut db_path = None;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--db" => {
                    index += 1;
                    db_path = args.get(index).cloned();
                }
                other => bail!("unknown argument: {other}"),
            }
            index += 1;
        }
        Ok(Self {
            text: String::new(),
            db_path,
            json: false,
        })
    }

    fn parse(args: &[String]) -> Result<Self> {
        let mut text = None;
        let mut db_path = None;
        let mut json = false;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--text" => {
                    index += 1;
                    text = args.get(index).cloned();
                }
                "--db" => {
                    index += 1;
                    db_path = args.get(index).cloned();
                }
                "--json" => json = true,
                other => bail!("unknown argument: {other}"),
            }
            index += 1;
        }
        let text = text.ok_or_else(|| anyhow::anyhow!("--text is required"))?;
        Ok(Self {
            text,
            db_path,
            json,
        })
    }
}

struct ServeOptions {
    db_path: Option<String>,
    port: Option<u16>,
}

impl ServeOptions {
    fn parse(args: &[String]) -> Result<Self> {
        let mut db_path = None;
        let mut port = None;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--db" => {
                    index += 1;
                    db_path = args.get(index).cloned();
                }
                "--port" => {
                    index += 1;
                    port = args.get(index).and_then(|value| value.parse().ok());
                }
                other => bail!("unknown argument: {other}"),
            }
            index += 1;
        }
        Ok(Self { db_path, port })
    }
}

fn print_help() {
    println!("agent-core-kernel run --text <message> [--db <path>] [--json]");
    println!("agent-core-kernel serve [--db <path>] [--port <port>]");
}
