# DevelopmentRequest attempt v21 Canary cutover

This is a quiesced, all-at-once cutover. It does not permit a v21 Kernel to
serve real requests with a v20 Coding Harness, or a v20 Kernel with a v21
Coding Harness. Do not use the currently polluted legacy Route Harness message
as a Canary; after cutover, use a new Feishu message.

## Preconditions

- Identify the exact v20 Kernel and Coding Harness binaries and the SQLite path.
- Prepare distinct backup paths for the v20 database and both v20 binaries.
- Confirm the v21 Kernel and Coding Harness were built from the same reviewed SHA.

## Canary cutover

1. Pause Canary ingress.
2. Wait until all current submission attempts have drained; do not terminate an active attempt.
3. Back up the v20 SQLite database with SQLite's online backup command (for example, `sqlite3 "$DB" ".backup '$DB_V20_BACKUP'"`) and copy the old Kernel and Coding Harness binaries. Verify the database copy with `PRAGMA integrity_check` and `PRAGMA user_version`, verify both binary copies, and record all three digests.
4. Stop both the old Kernel and old Coding Harness.
5. Execute the v21 migration against a database copy first, verify `PRAGMA user_version=21`, then execute the same migration against the quiesced Canary database. Migration failure must leave the database at complete v20 and may be rerun.
6. Start the new Coding Harness.
7. Start the new Kernel.
8. Run a local probe that only checks health, protocol classification, schema version, and attempt-store readability; it must not submit a DevelopmentRequest or invoke any external effect.
9. Restore Canary ingress and use a new Feishu message for the Canary submission.

If any binary version or health check is ambiguous, keep ingress paused. Never
route real requests through a mixed-version Kernel/Harness pair.

## Rollback

1. Pause Canary ingress.
2. Stop both the new Kernel and new Coding Harness.
3. Replace the v21 database with the verified v20 database backup; do not run a down-migration.
4. Restore both old v20 binaries.
5. Start the old Coding Harness and old Kernel, verify their versions, health, and the restored database's `PRAGMA user_version=20`.
6. Restore ingress only after verification succeeds.
