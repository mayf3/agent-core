fn main() {
    let config = match deployment_harness::config::DeploymentHarnessConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("deployment-harness configuration error: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = deployment_harness::server::serve(config) {
        eprintln!("deployment-harness stopped: {error}");
        std::process::exit(1);
    }
}
