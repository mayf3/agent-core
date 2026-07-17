//! Token Dashboard Acceptance Kit.
//!
//! Re-exports from public_spec and private_verifier modules.

mod public_spec;
mod private_verifier;

pub use public_spec::public_spec;
pub use private_verifier::verify;
