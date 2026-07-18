//! Binary entrypoint for the Development Controller.
//!
//! Bind address comes from `DEVELOPMENT_CONTROLLER_BIND_ADDR`
//! (default `127.0.0.1:7500`). The server is loopback-only.

use anyhow::Result;
use development_controller::{server::serve, ControllerConfig};

fn main() -> Result<()> {
    let config = ControllerConfig::from_env();
    serve(config)
}
