//! Stable DTO boundary for the Agent Core External Orchestration Seam V0.
//!
//! This crate is the *only* type surface shared between the Kernel and an
//! external Development Controller. It deliberately depends on nothing but
//! `serde`, `serde_json`, `sha2`, and `hex` — **never** on `agent-core-kernel`.
//!
//! What is allowed here (Seam V0):
//!   - `ProtocolVersion`
//!   - `InvocationId`, `RunId`, `PrincipalRef`
//!   - `OpaqueRef` (an opaque, digest-bearing content reference)
//!   - `Sha256Digest`
//!   - `ExternalOrchestrationIntent`
//!   - `ExternalOrchestrationResult`
//!   - `compute_result_digest`
//!
//! What is forbidden here (deferred to later milestones):
//!   - `IngressEnvelope`, Session internals, Gateway, Journal, Proposal,
//!     Approval, DeploymentIntent, DeploymentReceipt, ComponentControl, HCR,
//!     DevelopmentRequest, TargetKind, Acceptance Kit, ServiceManifest.
//!
//! The Kernel treats an `ExternalOrchestrationResult` strictly as the receipt
//! of one approved external invocation. It does NOT denote candidate
//! acceptance, capability approval, deployment success, or a registry effect.

mod digest;
mod refs;
mod result;
mod version;

pub use digest::{compute_result_digest, Sha256Digest};
pub use refs::{InvocationId, OpaqueRef, PrincipalRef, RunId};
pub use result::{ExternalOrchestrationIntent, ExternalOrchestrationResult, OrchestrationOutcome};
pub use version::{ProtocolVersion, PROTOCOL_VERSION};

pub use serde_json as _serde_json_export;
