//! HCR (HarnessChangeRequest) worker, evidence, settlement, and recovery.
//!
//! R3A adds:
//! - [`evidence::register_gate_evidence`] — durable gate evidence registration
//! - [`settlement::settle_hcr`] — atomic settlement from persisted evidence
//! - [`resume::determine_resume_state`] — evidence-based crash recovery
//!
//! R2 provides worker entry point and claim/binding infrastructure.

pub mod evidence;
pub mod resume;
pub mod revalidate;
pub mod settlement;
pub mod worker;
