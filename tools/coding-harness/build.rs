// Production safety guard: the `test-fixtures` Cargo feature must never be
// enabled in release/production builds. It is only valid during test
// compilation (`cargo test --features test-fixtures`).
//
// This build script:
//   - Rejects release builds that enable `test-fixtures` (hard error).
//   - Warns when `test-fixtures` is enabled in any non-test profile.
//
// The existing `#[cfg(feature = "test-fixtures")]` gating in the source
// provides the structural separation; this guard ensures the build chain
// cannot accidentally or intentionally produce a release artifact with
// test-only fixture symbols present.
//
// See also: HANDOVER §4.1, tools/coding-harness/src/fixtures/mod.rs

fn main() {
    let has_fixtures = std::env::var("CARGO_FEATURE_TEST_FIXTURES").is_ok();
    if !has_fixtures {
        return;
    }

    let profile = std::env::var("PROFILE").unwrap_or_default();

    if profile == "release" {
        panic!(
            "FATAL: test-fixtures feature is FORBIDDEN in release builds. \
             Use `cargo test --features test-fixtures` instead."
        );
    }

    // Warn in any non-release profile (debug, test, bench, etc.) that the
    // feature is active — developers should be aware.
    println!(
        "cargo:warning=test-fixtures feature is enabled (profile={}). \
         This is valid only for test contexts.",
        profile
    );
}
