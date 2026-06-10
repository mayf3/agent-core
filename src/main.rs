use agent_core_kernel::adapters::StdoutAdapter;
use agent_core_kernel::config::KernelConfig;
use agent_core_kernel::gateway::Gateway;
use agent_core_kernel::journal::JournalStore;
use agent_core_kernel::llm::LocalEchoLlm;
use agent_core_kernel::runtime::Runtime;
use anyhow::{bail, Result};
use serde_json::json;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("run") {
        print_help();
        return Ok(());
    }
    let options = CliOptions::parse(&args[1..])?;
    let config = KernelConfig::from_cli(options.db_path);
    let journal = JournalStore::open(&config.db_path)?;
    let gateway = Gateway::new(config.clone());
    let runtime = Runtime::new(config, LocalEchoLlm, StdoutAdapter);
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

struct CliOptions {
    text: String,
    db_path: Option<String>,
    json: bool,
}

impl CliOptions {
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

fn print_help() {
    println!("agent-core-kernel run --text <message> [--db <path>] [--json]");
}
