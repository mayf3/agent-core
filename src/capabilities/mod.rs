//! Tool capabilities — concrete implementations of ReadOnly operations that
//! the model may call. Each capability lives in its own file under this module
//! and exposes a public function (not on `Runtime`) so that
//! `handle_inline_tool_call` in the Runtime can dispatch to it without the
//! Runtime knowing the business logic of the operation.
//!
//! Adding a new capability:
//! 1. Declare a new constant + `OperationSpec` in `src/domain/operation.rs`.
//! 2. Implement the function in a new file under this module.
//! 3. Register a match arm in `Runtime::handle_inline_tool_call`.
//! 4. Do **not** add product-specific logic (keyword detection, formatting,
//!    emoji, natural-language logic) to the Runtime. The model decides when
//!    to call the capability based on the catalog context block.
//!
//! Capabilities are **not** adapters — they are inline functions that run in
//! the Runtime process and have access to the JournalStore. They return
//! structured JSON values as receipt output, which the model formats into
//! user-facing replies (which go through the normal outbox path).

pub mod store;
mod system_status;

pub use system_status::execute;
