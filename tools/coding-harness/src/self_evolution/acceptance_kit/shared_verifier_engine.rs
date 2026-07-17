//! Shared verification engine used by all Acceptance Kits.
//!
//! Changing this file affects the bundle digest of every kit that
//! depends on it (via build.rs per-kit bundle computation).

/// Format a structured constraint diagnostic for model-visible feedback.
///
/// This is the canonical form for communicating acceptance constraint
/// violations back to the model during repair rounds. The format is:
///
/// ```text
/// ACCEPTANCE_CONSTRAINT: <constraint_id>
/// PATH: <path>
/// EXPECTED: <expected>
/// ACTUAL: <actual>
/// ```
pub fn constraint_diagnostic(
    constraint_id: &str,
    path: &str,
    expected: &str,
    actual: &str,
) -> String {
    format!(
        "ACCEPTANCE_CONSTRAINT: {constraint_id}\nPATH: {path}\nEXPECTED: {expected}\nACTUAL: {actual}"
    )
}

/// Truncate diagnostics to a safe maximum length, preserving UTF-8 boundaries.
pub fn truncate_diagnostics(value: &str) -> String {
    let max_len = 16 * 1024;
    let mut end = value.len().min(max_len);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

/// Sanitize model diagnostics by replacing the generator root and candidate
/// id with safe placeholders, preventing host path disclosure.
pub fn sanitize_model_diagnostics(
    diagnostics: &str,
    base: &std::path::Path,
    candidate_id: &str,
) -> String {
    let root_repr = "<generator-root>";
    let id_repr = "<candidate-id>";
    let sanitized = diagnostics
        .replace(base.to_str().unwrap_or(""), root_repr)
        .replace(candidate_id, id_repr);
    sanitized
}
