# Security and Privacy

Agent Core is local-first, but it will interact with model APIs, Feishu, shell,
files, and external tools. Treat the repository as public by default.

## Never Commit

- API keys
- Feishu app secrets
- access tokens
- private keys
- `.env` files
- local run state
- logs containing prompts, tool outputs, or user data
- production config with real credentials

Use `.env.example` for documentation and keep real values outside git.

## Secret Handling

Secrets should enter runtime through environment variables or a local secret
store. The kernel records secret presence and config readiness, not secret values.

Allowed:

```text
FEISHU_APP_ID is configured
OPENAI_BASE_URL is configured
provider=openai-compatible
```

Not allowed:

```text
FEISHU_APP_SECRET=...
OPENAI_API_KEY=...
Authorization: Bearer ...
```

## Logging Policy

Default logs should store summaries, hashes, ids, and redacted previews.

Full prompts, tool outputs, and external messages require explicit debug mode.
Debug artifacts must stay local and ignored by git.

## Checks

Run before every PR:

```bash
pnpm check
```

The first guardrails are:

- repository structure limits
- basic local secret pattern scan

These checks are not a complete DLP system. They are a last-mile guardrail, not a
replacement for careful review.

## Feishu Credentials

Feishu configuration should be added only when the plugin is implemented.
Expected local variables may include:

```text
AGENT_CORE_FEISHU_APP_ID
AGENT_CORE_FEISHU_APP_SECRET
AGENT_CORE_FEISHU_CONNECTION_MODE
```

Use `.env.example` to document names without values.
