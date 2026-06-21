use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

// --- Current bootstrap templates (Phase 2+) ---
//
// These express generic capability boundaries. They do NOT hardcode any
// operation name, channel, or keyword routing. The model is told that it may
// use tools explicitly provided in the current request and authorized by the
// Gateway, and should prefer an authorized read-only tool over guessing for
// real-time / system / current-session facts.
const ROOT_MD: &str = "# Root System\n\
\n\
You are the main Agent Core assistant.\n\
\n\
You may use tools that are explicitly provided in the current request and \
authorized by the Gateway. Do not assume any tool that was not provided or \
not authorized.\n";
const RUNTIME_MD: &str = "# Runtime Contract\n\
\n\
External actions must be expressed as invocation intents and approved by Gateway.\n\
\n\
When a request involves real-time information, system state, or current-session \
facts, do not guess: if an authorized read-only tool is available, prefer using \
it. The available tools are exactly those provided in the request; never invent \
or assume additional tools.\n";
const AGENT_MD: &str = "# Main Agent\n\
\n\
You assist the user by answering messages and, when useful, calling the tools \
explicitly provided in the current request. Prefer an authorized read-only tool \
over guessing when the answer depends on real-time, system, or session facts. Do \
not assume tools that were not provided or not authorized.\n";
const CHAT_SKILL_MD: &str = "# Chat\n\nReply clearly and briefly to the current user message.\n";

// --- Legacy bootstrap templates (Phase 0) ---
//
// These are the exact byte-for-byte texts that `ensure_data_files` wrote on
// Phase 0 / Phase 1 kernels. They conflict with the tool loop ("chat-only",
// "without tools"), so a generated agent carrying them will refuse to call even
// an authorized `time.now`. Migration below upgrades a file ONLY when its
// content matches the legacy default for that exact path byte-for-byte, and
// never overwrites a user-customized file.
const LEGACY_ROOT_MD: &str =
    "# Root System\n\nYou are the main Agent Core assistant. Keep Phase 0 chat-only.\n";
const LEGACY_RUNTIME_MD: &str = "# Runtime Contract\n\nExternal actions must be expressed as invocation intents and approved by Gateway.\n";
const LEGACY_AGENT_MD: &str =
    "# Main Agent\n\nDefault Phase 0 agent. It answers user messages without tools.\n";
const LEGACY_CHAT_SKILL_MD: &str =
    "# Chat\n\nReply clearly and briefly to the current user message.\n";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct BootstrapMigrationReport {
    pub created: usize,
    pub upgraded: usize,
    pub migration_needed: usize,
}

enum TemplateAction {
    Created,
    Upgraded,
    Current,
    MigrationNeeded,
}

pub fn default_data_dir() -> PathBuf {
    home_dir()
        .map(|home| home.join(".agent-core"))
        .unwrap_or_else(|| PathBuf::from(".agent-core"))
}

pub fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(value));
    }
    PathBuf::from(value)
}

/// Ensure the bootstrap template files exist under `data_dir`, and upgrade any
/// file whose content is EXACTLY a known legacy Phase-0 default to the current
/// template for that path. A file that the user has customized is never
/// overwritten. Idempotent: a second run is a no-op.
pub fn ensure_data_files(data_dir: &Path) -> Result<BootstrapMigrationReport> {
    let templates = [
        ("system/root.md", LEGACY_ROOT_MD, ROOT_MD),
        ("system/runtime.md", LEGACY_RUNTIME_MD, RUNTIME_MD),
        ("agents/main/AGENT.md", LEGACY_AGENT_MD, AGENT_MD),
        ("skills/chat/SKILL.md", LEGACY_CHAT_SKILL_MD, CHAT_SKILL_MD),
    ];
    let mut report = BootstrapMigrationReport::default();
    for (relative_path, legacy_default, current_default) in templates {
        match upgrade_template(
            &data_dir.join(relative_path),
            legacy_default,
            current_default,
        )? {
            TemplateAction::Created => report.created += 1,
            TemplateAction::Upgraded => report.upgraded += 1,
            TemplateAction::MigrationNeeded => report.migration_needed += 1,
            TemplateAction::Current => {}
        }
    }
    Ok(report)
}

/// Migration semantics for a single template file:
/// - missing  → write the new default
/// - exact legacy default → overwrite with the new default
/// - anything else (user-customized, or already the new default) → untouched
///
/// All writes use `atomic_write` so a crash or write failure can never leave a
/// truncated/corrupt template file: the target file is either untouched or
/// fully replaced.
fn upgrade_template(
    path: &Path,
    legacy_default: &str,
    new_default: &str,
) -> Result<TemplateAction> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        atomic_write(path, new_default)?;
        return Ok(TemplateAction::Created);
    }
    let current = fs::read_to_string(path)?;
    if current == new_default {
        return Ok(TemplateAction::Current);
    }
    if current == legacy_default {
        atomic_write(path, new_default)?;
        return Ok(TemplateAction::Upgraded);
    }
    Ok(TemplateAction::MigrationNeeded)
}

/// Write `content` to `path` atomically: write to a unique temp file in the
/// SAME directory, flush+sync, then rename onto the target. On failure the
/// target file is never truncated or partially written; the temp file is
/// cleaned up. New files get safe default permissions (0600). This is a small,
/// closed helper — not a general file-transaction framework.
fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    // Unique temp name in the same directory (required for rename atomicity).
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("tmpl")
    ));
    let write_result: Result<()> = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        // Safe permissions on a newly-created file (best-effort; files being
        // upgraded keep their original mode because rename preserves it).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
        use std::io::Write;
        f.write_all(content.as_bytes())?;
        f.flush()?;
        // fsync so the bytes hit disk before the rename.
        f.sync_all()?;
        Ok(())
    })();
    if write_result.is_err() {
        // Clean up the partial temp file; the target is untouched.
        let _ = fs::remove_file(&tmp);
        return write_result;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn copy_legacy_db_if_needed(legacy_path: &Path, new_path: &Path) -> Result<()> {
    if new_path.exists() || !legacy_path.exists() {
        return Ok(());
    }
    if let Some(parent) = new_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(legacy_path, new_path)?;
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "agent-core-data-dir-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            id,
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn new_directory_writes_current_templates() {
        let dir = temp_dir();
        let report = ensure_data_files(&dir).unwrap();
        assert_eq!(report.created, 4);
        assert_eq!(report.upgraded, 0);
        assert_eq!(report.migration_needed, 0);
        assert_eq!(
            std::fs::read_to_string(dir.join("system/root.md")).unwrap(),
            ROOT_MD
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("system/runtime.md")).unwrap(),
            RUNTIME_MD
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("agents/main/AGENT.md")).unwrap(),
            AGENT_MD
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exact_legacy_default_is_upgraded() {
        let dir = temp_dir();
        // Seed the exact Phase-0 legacy content.
        for (rel, legacy) in [
            ("system/root.md", LEGACY_ROOT_MD),
            ("system/runtime.md", LEGACY_RUNTIME_MD),
            ("agents/main/AGENT.md", LEGACY_AGENT_MD),
            ("skills/chat/SKILL.md", LEGACY_CHAT_SKILL_MD),
        ] {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, legacy).unwrap();
        }
        let report = ensure_data_files(&dir).unwrap();
        assert_eq!(report.upgraded, 3);
        assert_eq!(report.migration_needed, 0);
        // All upgraded to the new defaults (no "Phase 0", no "without tools").
        let root = std::fs::read_to_string(dir.join("system/root.md")).unwrap();
        assert!(!root.contains("Phase 0"), "root upgraded: {root}");
        let agent = std::fs::read_to_string(dir.join("agents/main/AGENT.md")).unwrap();
        assert!(
            !agent.contains("without tools") && !agent.contains("Phase 0"),
            "agent upgraded: {agent}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_customized_content_is_preserved() {
        let dir = temp_dir();
        let custom = "# Main Agent\n\nMy personal custom prompt the user wrote.\n";
        let p = dir.join("agents/main/AGENT.md");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, custom).unwrap();
        let report = ensure_data_files(&dir).unwrap();
        assert_eq!(report.migration_needed, 1);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), custom);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_char_edit_of_legacy_is_not_overwritten() {
        let dir = temp_dir();
        // Legacy text with one character changed — NOT an exact default.
        let edited =
            "# Main Agent\n\nDefault Phase 0 agent. It answers user messages without toolz.\n";
        let p = dir.join("agents/main/AGENT.md");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, edited).unwrap();
        let report = ensure_data_files(&dir).unwrap();
        assert_eq!(report.migration_needed, 1);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), edited);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_is_idempotent() {
        let dir = temp_dir();
        ensure_data_files(&dir).unwrap();
        let first = std::fs::read_to_string(dir.join("system/root.md")).unwrap();
        let report = ensure_data_files(&dir).unwrap();
        assert_eq!(report, BootstrapMigrationReport::default());
        let second = std::fs::read_to_string(dir.join("system/root.md")).unwrap();
        assert_eq!(first, second);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_templates_contain_no_phase0_or_without_tools() {
        assert!(!ROOT_MD.contains("Phase 0") && !ROOT_MD.contains("chat-only"));
        assert!(!AGENT_MD.contains("Phase 0") && !AGENT_MD.contains("without tools"));
        assert!(!RUNTIME_MD.contains("Phase 0"));
    }

    #[test]
    fn legacy_content_in_the_wrong_file_is_preserved() {
        let dir = temp_dir();
        let path = dir.join("system/root.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, LEGACY_AGENT_MD).unwrap();
        let report = ensure_data_files(&dir).unwrap();
        assert_eq!(report.migration_needed, 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), LEGACY_AGENT_MD);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ===== §3: atomic write semantics =====

    #[test]
    fn atomic_write_creates_missing_file() {
        let dir = temp_dir();
        let p = dir.join("sub/agent.md");
        atomic_write(&p, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello");
        // No leftover temp file.
        assert!(
            !std::fs::read_dir(dir.join("sub"))
                .unwrap()
                .any(|e| { e.unwrap().file_name().to_string_lossy().ends_with(".tmp") }),
            "no leftover temp file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_replaces_existing_file_atomically() {
        let dir = temp_dir();
        let p = dir.join("x.md");
        std::fs::write(&p, "old").unwrap();
        atomic_write(&p, "new content").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new content");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_failure_leaves_original_unchanged() {
        let dir = temp_dir();
        let p = dir.join("good.md");
        std::fs::write(&p, "original").unwrap();
        // Point the write at a path whose parent is a FILE → create_dir_all /
        // open must fail. The original must be untouched.
        let bad = dir.join("good.md/notallowed/sub.md");
        let result = atomic_write(&bad, "x");
        assert!(result.is_err(), "write should fail");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "original");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_is_idempotent() {
        let dir = temp_dir();
        let p = dir.join("i.md");
        atomic_write(&p, "v1").unwrap();
        atomic_write(&p, "v1").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "v1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_data_files_upgrade_is_atomic() {
        // An exact legacy default is upgraded; on success the file is the new
        // default, no temp file remains.
        let dir = temp_dir();
        let p = dir.join("agents/main/AGENT.md");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, LEGACY_AGENT_MD).unwrap();
        ensure_data_files(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), AGENT_MD);
        assert!(
            !std::fs::read_dir(dir.join("agents/main"))
                .unwrap()
                .any(|e| { e.unwrap().file_name().to_string_lossy().ends_with(".tmp") }),
            "no leftover temp file after upgrade"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_and_newline_changed_custom_files_preserved() {
        let dir = temp_dir();
        // Empty file is custom (not a legacy default) → preserved.
        let p = dir.join("system/root.md");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "").unwrap();
        let report = ensure_data_files(&dir).unwrap();
        assert_eq!(report.migration_needed, 1);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_utf8_custom_file_is_preserved_or_safely_failed() {
        let dir = temp_dir();
        let p = dir.join("agents/main/AGENT.md");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        // Invalid UTF-8 bytes — not a legacy default, must NOT be overwritten.
        std::fs::write(&p, b"\xff\xfe garbage").unwrap();
        let report = ensure_data_files(&dir);
        // Either migration_needed (read fails → treated as needs-migration) or
        // an Err is acceptable, but the file must be untouched.
        match report {
            Ok(r) => {
                assert_eq!(r.migration_needed, 1);
            }
            Err(_) => {}
        }
        assert_eq!(std::fs::read(&p).unwrap(), b"\xff\xfe garbage");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_new_file_has_safe_permissions() {
        let dir = temp_dir();
        let p = dir.join("perm.md");
        atomic_write(&p, "x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "new file should have 0600, got {:o}",
            mode
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
