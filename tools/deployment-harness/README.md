# Deployment Harness

This external process owns managed-service installation, start, health checks,
upgrade switch, disable, and rollback. It accepts only strict
`deployment.effect.v0` intents and loads both service manifests and executable
bytes from the shared content-addressed store.

See [the operations runbook](../../docs/ops/deployment-harness.md) and
[the lifecycle architecture](../../docs/architecture/managed-service-lifecycle-v0.md).
