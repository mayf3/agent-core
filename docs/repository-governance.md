# Repository Governance

## Purpose

The repository should be safe to push to a personal remote and easy to audit over
time. Every feature should leave a readable trail: document, branch, PR, merge,
and check output.

## Required Rules

- Keep `main` protected by habit first, branch protection later.
- Use PRs for all changes after bootstrap.
- Reviews are optional for now.
- Run `pnpm check` before opening or merging a PR.
- Do not commit secrets, local state, or raw sensitive logs.
- Keep generated runtime artifacts out of git.

## Bootstrap Exception

Because the repository started as an empty local directory, the initial commit may
be created directly. After the first push to the remote, all further changes use
branches and PRs.

## Traceability Records

Each PR should preserve:

```text
problem
decision
files changed
checks run
known risks
next step
```

This keeps architecture decisions inspectable without building a heavy process.

## Remote Setup

The remote should be the user's personal repository. The URL is not stored in
docs because repository hosting can change.

Suggested setup once the repo exists:

```bash
git init
git branch -M main
git remote add origin <personal-repo-url>
git push -u origin main
```

For later changes:

```bash
git switch -c docs/example-change
pnpm check
git add .
git commit -m "docs: describe example change"
git push -u origin docs/example-change
```

Then open and merge a PR on the remote.
