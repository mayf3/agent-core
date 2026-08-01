//! Agent Loop Harness V0 — binary entry point.
//!
//! External process that observes Kernel Run outcomes and, when the V0 policy
//! says `continue_same_session`, requests the next Run in the same session.
//! See `lib.rs` for the architecture and `README.md` for configuration.

fn main() -> anyhow::Result<()> {
    let config = agent_loop_harness::config_from_env()?;
    eprintln!(
        "agent-loop-harness: observing {} (state: {})",
        config.kernel_url,
        config.state_path.display()
    );
    agent_loop_harness::run_forever(config)
}
