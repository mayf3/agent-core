//! Entry point for the `component-deploy-adapter` binary — an independent
//! userspace process (NOT part of the Deployment Harness frozen core) that
//! exposes `external.component_deploy` on the `external-harness-v1` protocol and
//! translates it to the Deployment Harness's unchanged `deployment.effect.v0`
//! deploy route. See `src/component_deploy_adapter.rs` for the boundary contract.

use std::net::TcpListener;

fn main() {
    let config = match deployment_harness::component_deploy_adapter::AdapterConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("component-deploy-adapter config error: {error}");
            std::process::exit(1);
        }
    };
    let listener = match TcpListener::bind(&config.listen_addr) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("component-deploy-adapter bind {addr} failed: {error}", addr = config.listen_addr);
            std::process::exit(1);
        }
    };
    eprintln!(
        "component-deploy-adapter listening on {addr}",
        addr = config.listen_addr
    );
    if let Err(error) =
        deployment_harness::component_deploy_adapter::serve_listener(listener, config)
    {
        eprintln!("component-deploy-adapter serve error: {error}");
        std::process::exit(1);
    }
}
