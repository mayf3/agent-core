# Contributing

Agent Core uses a PR-first history even for solo work. Reviews are not required
for now, but every change should be traceable through a branch, pull request, and
merge commit or squash merge.

## Change Flow

```text
create branch
  -> make changes
  -> run pnpm check
  -> commit
  -> push branch
  -> open PR
  -> merge PR
```

Direct commits to `main` are only acceptable for repository bootstrap before the
remote exists.

## Branch Names

Use short descriptive names:

```text
docs/architecture-governance
feat/feishu-plugin
chore/secret-scan
fix/tool-policy
```

## Commit Rules

- Keep commits focused.
- Do not commit secrets, local state, logs, or generated runtime artifacts.
- Include docs when changing architecture or policy.
- Run `pnpm check` before pushing.

## Pull Requests

Each PR should include:

- What changed.
- Why it changed.
- Verification performed.
- Any risk or follow-up.

Formal review is optional until the project needs multiple maintainers, but PRs
remain required for traceability.

## Remote Repository

The remote URL is intentionally not hard-coded. Once the personal repository
exists, configure it locally:

```bash
git remote add origin <personal-repo-url>
git push -u origin main
```

After that, use branches and PRs for every change.
