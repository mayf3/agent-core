//! HCR (Harness Change Request) secure execution profile.
//!
//! This module provides the secure execution profile for Coding Harness
//! HCR operations. It implements:
//!
//! - **Command policy**: Structured argv-only execution, no shell, no eval,
//!   with named command templates and allowlisting.
//! - **Environment isolation**: `env_clear()` + allowlist, sandbox HOME.
//! - **Filesystem sandbox**: macOS `sandbox-exec` / Linux `bubblewrap`,
//!   with `fail closed` on unavailable backend.
//! - **Network policy**: Per-command deny/loopback-only.
//! - **Process lifecycle**: Timeout, process group kill, output truncation.
//! - **Structured results**: Consistent JSON envelope with status,
//!   exit code, truncation flags, and cleanup confirmation.

pub mod candidate;
pub mod command;
pub mod errors;
pub mod executor;
pub mod gates;
pub mod process;
pub mod profile;
pub mod sandbox;
