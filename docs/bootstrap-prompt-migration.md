# Bootstrap Prompt Migration

## Background

Phase-0 / Phase-1 kernels wrote bootstrap templates that said "Keep Phase 0
chat-only" and "answers user messages without tools". These prompts conflict
with the Phase-2 tool loop: a generated agent carrying them refuses to call even
an authorized external.time_now harness (builtin time.now was retired in PR #165).

The Kernel now ships corrected templates (`src/data_dir.rs`) and a **precise,
non-destructive migration** that runs on every startup against the configured
Agent Core home (`AGENT_CORE_DATA_DIR`, default `~/.agent-core`).

## Migration rules

For each bootstrap file (`system/root.md`, `system/runtime.md`,
`agents/main/AGENT.md`, `skills/chat/SKILL.md`):

| File state | Behavior |
|---|---|
| Missing | Write the new default template |
| Content is EXACTLY a known Phase-0 default (byte-for-byte) | Overwrite with the new default |
| Content is already the new default | No-op (idempotent) |
| Content differs from any known default (user-customized) | **Left untouched** |

Matching is exact, path-specific, and byte-for-byte, not fuzzy and not a digest.
A Root template placed in the Agent template path is not considered that path's
legacy default. Editing a single character of a legacy default makes it
"user-customized" and it will NOT be upgraded.

All template writes (new-file creation and legacy upgrade) are atomic: a temp
file is written in the same directory, flushed, fsynced, then renamed onto the
target. A crash or write failure never truncates a template; on failure the
original is untouched and the temp file is cleaned up. New files use safe
default permissions (0600 on unix).

## Recognizing the old Phase-0 Prompt

If your `agents/main/AGENT.md` (or `system/root.md`) contains:

```text
Keep Phase 0 chat-only
... without tools ...
Default Phase 0 agent
```

then it is a legacy default and will be upgraded automatically on next Kernel
start. If you edited it yourself and it still mentions "Phase 0" but does not
match the exact default, it is treated as custom — you must update it manually
to remove the "chat-only" / "without tools" framing.

## Your custom Prompt is never overwritten

The migration ONLY upgrades files whose bytes exactly match a known legacy
default. Any user customization — even one character — is preserved. Repeated
Kernel restarts are idempotent (a file already at the new default is a no-op).

When customized templates are preserved, startup emits only a bounded
`bootstrap_prompt_migration_needed` status and a count. It does not log file
contents or scan outside the explicitly configured Agent Core data directory.
