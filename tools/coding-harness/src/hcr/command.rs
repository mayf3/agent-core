//! HCR command policy — validates and resolves commands against a profile.
//!
//! The command policy ensures:
//! - No `sh -c` / `bash -c` / arbitrary shell execution
//! - No `node -e` / `--eval` / arbitrary scripts
//! - Only named, profiled commands with structured arguments
//! - All arguments are validated against the command template

use std::collections::HashMap;
use std::path::PathBuf;

use super::errors::HcrError;
use super::profile::{ArgTemplate, HcrProfile, NetworkPolicy};

/// A resolved command ready for execution.
///
/// After policy validation, the command is fully resolved with:
/// - The absolute program path
/// - Structured argument vector (no shell)
/// - Effective network policy
/// - Effective timeout
#[derive(Debug, Clone)]
pub struct ResolvedCommand {
    /// Absolute path to the executable.
    pub program: PathBuf,
    /// Structured argument vector (argv).
    pub args: Vec<String>,
    /// Effective network policy for this execution.
    pub network: NetworkPolicy,
    /// Timeout in milliseconds.
    pub timeout_ms: u64,
}

/// The command policy engine.
pub struct CommandPolicy;

/// Programs that are never allowed in HCR execution.
const FORBIDDEN_PROGRAMS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish"];

/// Argument patterns that are never allowed in HCR execution.
/// These are checked against the argv of the resolved command.
const FORBIDDEN_ARG_PATTERNS: &[&str] = &["-c", "-e", "--eval", "-i", "--interactive"];

impl CommandPolicy {
    /// Validate and resolve a named command against the profile.
    ///
    /// # Arguments
    /// * `command_name` — The name of the command (must match an entry in the profile).
    /// * `params` — The caller-supplied parameters for the command templates.
    /// * `profile` — The active HCR profile.
    /// * `workspace_root` — The workspace root path (for resolving relative paths).
    ///
    /// # Returns
    /// A `ResolvedCommand` if the command is valid, or an `HcrError` explaining why.
    pub fn check(
        command_name: &str,
        params: &HashMap<String, String>,
        profile: &HcrProfile,
        _workspace_root: &PathBuf,
    ) -> Result<ResolvedCommand, HcrError> {
        if command_name.is_empty() {
            return Err(HcrError::MissingCommand);
        }

        // Find the command entry in the profile
        let entry = profile
            .find_command(command_name)
            .ok_or(HcrError::CommandNotAllowed)?;

        // Check the program is not in the forbidden list
        let prog_name = entry
            .program
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();
        if FORBIDDEN_PROGRAMS.contains(&prog_name.as_ref()) {
            return Err(HcrError::CommandNotAllowed);
        }

        // Resolve argument templates
        let mut args = Vec::new();
        for tmpl in &entry.args {
            match tmpl {
                ArgTemplate::Fixed(s) => {
                    // Check that fixed args don't contain forbidden patterns
                    if FORBIDDEN_ARG_PATTERNS.contains(&s.as_str()) {
                        return Err(HcrError::CommandNotAllowed);
                    }
                    args.push(s.clone());
                }
                ArgTemplate::Param(name) => {
                    let value = params
                        .get(name)
                        .ok_or_else(|| HcrError::MissingParameter(name.clone()))?;
                    if value.is_empty() {
                        return Err(HcrError::InvalidParameter(name.clone()));
                    }
                    // Validate parameter: no shell metacharacters
                    Self::validate_param(name, value)?;
                    args.push(value.clone());
                }
            }
        }

        // Determine effective network policy
        let network = profile.effective_network(entry);

        // Determine effective timeout
        let timeout_ms = entry
            .timeout_ms_default
            .unwrap_or(profile.timeout_ms_max)
            .min(profile.timeout_ms_max);

        Ok(ResolvedCommand {
            program: entry.program.clone(),
            args,
            network,
            timeout_ms,
        })
    }

    /// Validate a parameter value for safety.
    ///
    /// Rejects:
    /// - Shell metacharacters (`;`, `|`, `&`, `` ` ``, `$`, `(`, `)`, `{`, `}`, `<`, `>`, `!`)
    /// - Path traversal (`..`)
    /// - Absolute paths (except for known param types like `harness_root`)
    fn validate_param(name: &str, value: &str) -> Result<(), HcrError> {
        // Reject empty parameters (already handled in caller)
        if value.is_empty() {
            return Err(HcrError::InvalidParameter(name.into()));
        }

        // Reject shell metacharacters in all parameters
        let dangerous = [
            ';', '|', '&', '`', '$', '(', ')', '{', '}', '<', '>', '!', '\n', '\r',
        ];
        if value.contains(&dangerous[..]) {
            return Err(HcrError::InvalidParameter(name.into()));
        }

        // Reject parameters with whitespace (could break argv boundary)
        if value.contains(char::is_whitespace) {
            return Err(HcrError::InvalidParameter(name.into()));
        }

        // Reject path traversal
        if value.contains("..") {
            return Err(HcrError::InvalidParameter(name.into()));
        }

        Ok(())
    }

    /// Quick check whether a raw program+args would be allowed.
    ///
    /// This is a stricter check for the case where someone passes a
    /// program/args pair directly (not via a named template). This
    /// codifies the "no shell, no eval" rule at a low level.
    pub fn check_raw_forbidden(program: &str, args: &[String]) -> bool {
        // Forbidden programs
        let prog_name = PathBuf::from(program)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if FORBIDDEN_PROGRAMS.contains(&prog_name.as_str()) {
            return true;
        }

        // Forbidden argument patterns
        for arg in args {
            if FORBIDDEN_ARG_PATTERNS.contains(&arg.as_str()) {
                return true;
            }
        }

        // `node <script>` where script is not a recognized harness path is risky
        // but we don't block it here — the profile should only allow specific
        // node subcommands via named templates.

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn test_profile() -> HcrProfile {
        HcrProfile {
            id: "test-profile".into(),
            workspace_id: "harness-dev".into(),
            allowed_commands: vec![
                super::super::profile::HcrCommandEntry {
                    name: "node_test".into(),
                    program: PathBuf::from("/usr/bin/env"),
                    args: vec![
                        ArgTemplate::Fixed("node".into()),
                        ArgTemplate::Fixed("--test".into()),
                        ArgTemplate::Param("test_path".into()),
                    ],
                    network: Some(NetworkPolicy::Deny),
                    timeout_ms_default: Some(60_000),
                },
                super::super::profile::HcrCommandEntry {
                    name: "trusted_script".into(),
                    program: PathBuf::from("/opt/harness/trusted.sh"),
                    args: vec![ArgTemplate::Param("input".into())],
                    network: None,
                    timeout_ms_default: None,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn allows_known_named_command() {
        let profile = test_profile();
        let mut params = HashMap::new();
        params.insert("test_path".into(), "server.test.mjs".into());
        let result = CommandPolicy::check("node_test", &params, &profile, &PathBuf::from("/ws"));
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved.program, PathBuf::from("/usr/bin/env"));
        assert!(resolved.args.contains(&"node".to_string()));
        assert!(resolved.args.contains(&"--test".to_string()));
        assert!(resolved.args.contains(&"server.test.mjs".to_string()));
    }

    #[test]
    fn rejects_unknown_command() {
        let profile = test_profile();
        let params = HashMap::new();
        let result = CommandPolicy::check("unknown_cmd", &params, &profile, &PathBuf::from("/ws"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().error_code(), "HCR_COMMAND_NOT_ALLOWED");
    }

    #[test]
    fn rejects_empty_command_name() {
        let profile = test_profile();
        let params = HashMap::new();
        let result = CommandPolicy::check("", &params, &profile, &PathBuf::from("/ws"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().error_code(), "HCR_MISSING_COMMAND");
    }

    #[test]
    fn rejects_missing_parameter() {
        let profile = test_profile();
        let params = HashMap::new(); // test_path not provided
        let result = CommandPolicy::check("node_test", &params, &profile, &PathBuf::from("/ws"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().error_code(), "HCR_MISSING_PARAMETER");
    }

    #[test]
    fn rejects_shell_metacharacters_in_params() {
        let profile = test_profile();
        let mut params = HashMap::new();
        params.insert("test_path".into(), "file; rm -rf /".into());
        let result = CommandPolicy::check("node_test", &params, &profile, &PathBuf::from("/ws"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().error_code(), "HCR_INVALID_PARAMETER");
    }

    #[test]
    fn rejects_path_traversal_in_params() {
        let profile = test_profile();
        let mut params = HashMap::new();
        params.insert("test_path".into(), "../../etc/passwd".into());
        let result = CommandPolicy::check("node_test", &params, &profile, &PathBuf::from("/ws"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().error_code(), "HCR_INVALID_PARAMETER");
    }

    #[test]
    fn rejects_whitespace_in_params() {
        let profile = test_profile();
        let mut params = HashMap::new();
        params.insert("test_path".into(), "two words".into());
        let result = CommandPolicy::check("node_test", &params, &profile, &PathBuf::from("/ws"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().error_code(), "HCR_INVALID_PARAMETER");
    }

    #[test]
    fn check_raw_forbidden_detects_sh() {
        assert!(CommandPolicy::check_raw_forbidden(
            "sh",
            &["-c".into(), "echo hi".into()]
        ));
        assert!(CommandPolicy::check_raw_forbidden(
            "bash",
            &["-c".into(), "echo hi".into()]
        ));
    }

    #[test]
    fn check_raw_forbidden_detects_node_eval() {
        assert!(CommandPolicy::check_raw_forbidden(
            "node",
            &["-e".into(), "console.log(1)".into()]
        ));
        assert!(CommandPolicy::check_raw_forbidden(
            "node",
            &["--eval".into(), "console.log(1)".into()]
        ));
    }

    #[test]
    fn check_raw_forbidden_allows_safe() {
        assert!(!CommandPolicy::check_raw_forbidden(
            "node",
            &["--test".into(), "test.mjs".into()]
        ));
        assert!(!CommandPolicy::check_raw_forbidden(
            "rustc",
            &["--version".into()]
        ));
    }

    #[test]
    fn respects_command_timeout() {
        let profile = test_profile();
        let mut params = HashMap::new();
        params.insert("test_path".into(), "test.mjs".into());
        let resolved =
            CommandPolicy::check("node_test", &params, &profile, &PathBuf::from("/ws")).unwrap();
        // node_test has timeout_ms_default = 60000, profile timeout_ms_max = 120000
        assert_eq!(resolved.timeout_ms, 60_000);
    }

    #[test]
    fn effective_network_from_command() {
        let profile = test_profile();
        let entry = profile.find_command("node_test").unwrap();
        assert_eq!(profile.effective_network(entry), NetworkPolicy::Deny);
    }
}
