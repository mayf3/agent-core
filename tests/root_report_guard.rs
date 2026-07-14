//! Repository-policy integration tests for `scripts/check-no-root-reports.sh`.
//!
//! Keeping these checks in the Rust test suite means the guard runs anywhere
//! `cargo test` runs, without depending on a specific hosted CI provider.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new(label: &str) -> std::io::Result<Self> {
        let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "agent core root report guard {label} {} {counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn run_guard(root: &Path) -> std::io::Result<Output> {
    Command::new("bash")
        .arg(format!(
            "{}/scripts/check-no-root-reports.sh",
            env!("CARGO_MANIFEST_DIR")
        ))
        .arg(root)
        .output()
}

fn assert_forbidden(filename: &str) -> anyhow::Result<()> {
    let root = TempRoot::new("forbidden")?;
    let report = root.path.join(filename);
    std::fs::write(&report, "temporary report")?;

    let output = run_guard(&root.path)?;
    assert!(!output.status.success(), "{filename} must fail the guard");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(filename), "diagnostic must name {filename}");
    assert!(report.exists(), "the guard must never delete files");
    Ok(())
}

#[test]
fn current_repository_passes_root_report_guard() -> anyhow::Result<()> {
    let output = run_guard(Path::new(env!("CARGO_MANIFEST_DIR")))?;
    assert!(
        output.status.success(),
        "current repository failed guard: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn all_forbidden_root_report_suffixes_fail_without_deletion() -> anyhow::Result<()> {
    assert_forbidden("TEST_AUDIT_REPORT.md")?;
    assert_forbidden("TEST_IMPLEMENTATION_REPORT.md")?;
    assert_forbidden("TEST_INVESTIGATION_REPORT.md")?;
    Ok(())
}

#[test]
fn nested_reports_and_git_metadata_are_not_scanned() -> anyhow::Result<()> {
    let root = TempRoot::new("nested")?;
    let docs = root.path.join("docs");
    let git = root.path.join(".git");
    std::fs::create_dir_all(&docs)?;
    std::fs::create_dir_all(&git)?;
    let docs_report = docs.join("ARCHITECTURE_AUDIT_REPORT.md");
    let git_report = git.join("INTERNAL_AUDIT_REPORT.md");
    std::fs::write(&docs_report, "permanent architecture record")?;
    std::fs::write(&git_report, "git metadata fixture")?;

    let output = run_guard(&root.path)?;
    assert!(output.status.success());
    assert!(docs_report.exists(), "nested docs must be preserved");
    assert!(git_report.exists(), ".git contents must be preserved");
    Ok(())
}

#[test]
fn repository_root_with_spaces_is_supported() -> anyhow::Result<()> {
    let root = TempRoot::new("path with spaces")?;
    let output = run_guard(&root.path)?;
    assert!(output.status.success());
    Ok(())
}
