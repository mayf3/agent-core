//! Production isolation tests for test-fixtures.
//!
//! Verifies that the `test-fixtures` feature cannot be accidentally enabled
//! in production builds and that fixture symbols are absent from release
//! artifacts.
//!
//! # Namespace scope
//!
//! `hook_consumer` names TWO distinct modules:
//!
//! - `coding_harness::fixtures::hook_consumer` — deterministic test fixture,
//!   gated behind `#[cfg(feature = "test-fixtures")]` in
//!   `src/fixtures/mod.rs`. Must never appear in release artifacts.
//! - `coding_harness::self_evolution::generator::hook_consumer` — production
//!   model-driven generator for `HookConsumerService` development requests.
//!   Required in release builds by design (see `src/fixtures/mod.rs`: "real
//!   Token Dashboard requests always go through the model generator").
//!
//! The isolation assertions therefore target the fixture namespace only. A
//! bare `hook_consumer` substring check would incorrectly flag the
//! production generator as a fixture leak.
//!
//! # Fail-closed artifact selection
//!
//! The release directory contains both `libcoding_harness.rlib` and
//! `libcoding_harness.d` (a Makefile-style dependency file). `nm` cannot
//! read the `.d` file, so selecting it and silently treating its empty
//! output as "no fixture symbols" would make the isolation check pass even
//! when the fixture module leaked into the build. The helpers below:
//!
//! - select only real `.rlib` artifacts (canonical `libcoding_harness.rlib`
//!   preferred; otherwise exactly one `libcoding_harness*.rlib` match);
//! - fail when no rlib is found or when several candidates make the choice
//!   ambiguous (never rely on directory traversal order);
//! - assert that `nm` itself succeeded, failing with the artifact path,
//!   exit code and stderr otherwise.
//!
//! # Positive control
//!
//! `test_fixtures_feature_build_contains_fixture_symbols` proves that the
//! `8fixtures13hook_consumer` pattern still matches a build that
//! deliberately enables `test-fixtures`, using a dedicated target directory
//! so it can never read a stale rlib from a different feature combination.
//! If rustc mangling ever changed such that the pattern matched nothing,
//! the isolation tests could otherwise pass vacuously forever.
//!
//! Run: `cargo test --test fixture_production_isolation`

use std::path::{Path, PathBuf};
use std::process::Command;

/// Mangled encoding of the `coding_harness::fixtures::hook_consumer`
/// namespace. rustc encodes path segments as `{length}{identifier}`
/// (`fixtures` = 8, `hook_consumer` = 13) in both the legacy (`_ZN...E`)
/// and v0 (`_RNv...`) mangling schemes, so this fragment uniquely
/// identifies the fixture namespace in `nm` output without a
/// platform-specific `--demangle` flag (GNU nm supports it; macOS nm does
/// not). It must not match the production generator namespace
/// (`...14self_evolution9generator13hook_consumer`), which is allowed in
/// release builds.
const FIXTURE_NAMESPACE_MANGLED: &str = "8fixtures13hook_consumer";

/// Filename prefix of the coding-harness library artifact.
const HARNESS_LIB_PREFIX: &str = "libcoding_harness";

fn contains_fixture_namespace(symbols: &str) -> bool {
    symbols.contains(FIXTURE_NAMESPACE_MANGLED)
}

/// Helper: return the target directory for the coding-harness crate.
fn target_dir() -> PathBuf {
    // Use cargo metadata to get the authoritative target directory.
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata should work");
    assert!(output.status.success(), "cargo metadata failed");
    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid cargo metadata JSON");
    PathBuf::from(
        meta["target_directory"]
            .as_str()
            .expect("target_directory present"),
    )
}

/// Run `cargo` with `args` from the coding-harness manifest dir.
fn cargo(args: &[&str]) -> std::process::ExitStatus {
    Command::new("cargo")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("cargo should run")
}

/// True if `path` is a coding-harness library artifact with the strict
/// `.rlib` extension. Makefile dependency files (`libcoding_harness.d`) and
/// any other non-object products are rejected: `nm` cannot read them, and
/// an empty `nm` output must never be interpreted as "no fixture symbols".
fn is_harness_rlib(path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    name.starts_with(HARNESS_LIB_PREFIX)
        && path
            .extension()
            .map(|ext| ext == "rlib")
            .unwrap_or(false)
}

/// Deterministic, sorted listing of `dir` for failure messages.
fn dir_listing(dir: &Path) -> String {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.join("\n")
}

/// Locate the coding-harness rlib inside `profile_dir` (fail-closed).
///
/// Preference order:
/// 1. the canonical `libcoding_harness.rlib`;
/// 2. otherwise the single top-level artifact matching
///    `libcoding_harness*.rlib`.
///
/// Zero candidates panics with the directory listing instead of guessing;
/// more than one candidate panics with the full candidate list rather than
/// relying on directory traversal order.
fn find_harness_rlib(profile_dir: &Path) -> PathBuf {
    let canonical = profile_dir.join(format!("{HARNESS_LIB_PREFIX}.rlib"));
    if canonical.is_file() {
        return canonical;
    }
    let candidates: Vec<PathBuf> = std::fs::read_dir(profile_dir)
        .unwrap_or_else(|e| {
            panic!(
                "cannot read profile dir {}: {e}",
                profile_dir.display()
            )
        })
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| is_harness_rlib(p))
        .collect();
    assert!(
        !candidates.is_empty(),
        "no coding-harness rlib found in {} — expected at least {}; \
         directory contents:\n{}",
        profile_dir.display(),
        canonical.display(),
        dir_listing(profile_dir)
    );
    assert!(
        candidates.len() == 1,
        "multiple coding-harness rlib candidates in {} — refusing to pick by \
         directory order:\n{}",
        profile_dir.display(),
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
    candidates.into_iter().next().expect("exactly one candidate")
}

/// Run `nm` on `path` and return its (stdout, stderr). Fails the test if
/// `nm` itself fails (fail-closed): a non-zero `nm` exit means the artifact
/// could not be inspected and must never count as "no fixture symbols".
fn nm_output(path: &Path) -> (String, String) {
    let output = Command::new("nm")
        .arg(path)
        .output()
        .expect("nm must be available");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "nm failed on {} — exit code {:?}:\n{}",
        path.display(),
        output.status.code(),
        stderr
    );
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr.into_owned(),
    )
}

/// Release build with `test-fixtures` must be REJECTED by the build.rs guard.
#[test]
fn release_build_rejects_test_fixtures() {
    let status = cargo(&[
        "build",
        "--release",
        "--lib",
        "--features",
        "test-fixtures",
        "--manifest-path",
        concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
    ]);
    assert!(
        !status.success(),
        "release build with test-fixtures must FAIL (build.rs guard)"
    );
}

/// Build the default (no-features) release lib and assert its canonical
/// rlib is free of fixture namespace symbols. Shared by both release
/// isolation tests so the fail-closed rlib/`nm` rules apply identically.
fn assert_default_release_lib_has_no_fixture_symbols() {
    let status = cargo(&[
        "build",
        "--release",
        "--lib",
        "--manifest-path",
        concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
    ]);
    assert!(status.success(), "default release build must succeed");

    let release_dir = target_dir().join("release");
    let rlib = find_harness_rlib(&release_dir);
    let (stdout, stderr) = nm_output(&rlib);

    let matching: Vec<&str> = stdout
        .lines()
        .filter(|line| contains_fixture_namespace(line))
        .collect();
    assert!(
        matching.is_empty(),
        "release rlib {} contains fixtures::hook_consumer symbols — \
         test-fixtures leaked! Matching nm lines:\n{}",
        rlib.display(),
        matching.join("\n")
    );
    // nm on macOS .rlib archives prints the archive member paths — the
    // fixture module object file would appear as a member name.
    assert!(
        !contains_fixture_namespace(&stderr),
        "nm stderr for release rlib {} mentions the fixture namespace: {}",
        rlib.display(),
        stderr
    );
}

/// Default release build (no test-fixtures) must succeed and produce a
/// clean artifact without fixture symbols. Production generator symbols
/// (e.g. `self_evolution::generator::hook_consumer`) may be present.
#[test]
fn release_build_has_no_fixture_symbols() {
    assert_default_release_lib_has_no_fixture_symbols();
}

/// A normal (debug) build without test-fixtures succeeds.
#[test]
fn default_build_succeeds() {
    let status = cargo(&[
        "build",
        "--lib",
        "--manifest-path",
        concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
    ]);
    assert!(status.success(), "default build must succeed");
}

/// No environment variable can enable test-fixtures in a release build.
#[test]
fn no_env_var_enables_test_fixtures() {
    // The only mechanism is --features test-fixtures at the Cargo level.
    // Verify that a build with no --features produces no fixture symbols.
    assert_default_release_lib_has_no_fixture_symbols();
}

/// Positive control: a non-release build with `--features test-fixtures`
/// MUST contain fixture namespace symbols in its rlib.
///
/// Proves only that the fixture module is compiled in when the feature is
/// enabled (`TEST_FIXTURE_MODULE_AVAILABLE_WITH_FEATURE=true`) and that the
/// `8fixtures13hook_consumer` pattern still matches real rustc mangling.
/// It does NOT run the fixture end-to-end: the full
/// `hook_consumer_gates_e2e` acceptance remains a separate, non-default
/// feature-gated test with its own tracking.
///
/// The feature build uses its own target directory (`--target-dir`) so it
/// can never read a stale rlib produced by a different feature combination.
#[test]
fn test_fixtures_feature_build_contains_fixture_symbols() {
    let feature_target = target_dir().join("fixture-isolation-feature");
    let status = cargo(&[
        "build",
        "--lib",
        "--features",
        "test-fixtures",
        "--target-dir",
        feature_target.to_str().expect("utf-8 target dir"),
        "--manifest-path",
        concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
    ]);
    assert!(
        status.success(),
        "debug build with test-fixtures must succeed (build.rs guard only \
         forbids the release profile)"
    );

    let rlib = find_harness_rlib(&feature_target.join("debug"));
    let (stdout, _stderr) = nm_output(&rlib);

    let matching: Vec<&str> = stdout
        .lines()
        .filter(|line| contains_fixture_namespace(line))
        .collect();
    assert!(
        !matching.is_empty(),
        "feature-build rlib {} contains NO fixtures::hook_consumer symbols — \
         the isolation pattern '{}' no longer matches rustc mangling, or the \
         fixture module is not compiled in. First nm lines:\n{}",
        rlib.display(),
        FIXTURE_NAMESPACE_MANGLED,
        stdout.lines().take(40).collect::<Vec<_>>().join("\n")
    );
}
