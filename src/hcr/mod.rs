//! HCR (HarnessChangeRequest) worker and revalidation.
//!
//! R2 adds atomic claim, trusted Run binding, and service-side revalidation
//! for HCR execution. R3 will add settle logic; R4 will add final Feishu reply.
//!
//! This module provides:
//! - [`revalidate::revalidate_hcr_context`] — server-side revalidation before
//!   each privileged tool dispatch.
//! - [`worker::execute_hcr`] — minimal HCR worker entry point.

pub mod revalidate;
pub mod worker;
