use agent_core_kernel::journal::JournalStore;
use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;

/// A fresh database is migrated and stamped with the current schema version.
#[test]
fn fresh_database_is_stamped_with_current_schema_version() -> Result<()> {
    let journal = JournalStore::in_memory()?;
    assert_eq!(
        journal.schema_version()?,
        14,
        "a fresh database must be stamped with the current schema version (14)"
    );
    Ok(())
}

/// Re-opening an existing at-version database succeeds and keeps the version stamp.
#[test]
fn existing_at_version_database_reopens_cleanly() -> Result<()> {
    let db_path = unique_temp_path();
    {
        let _journal = JournalStore::open(&db_path)?;
    }
    let journal = JournalStore::open(&db_path)?;
    assert_eq!(journal.schema_version()?, 14);
    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// A real v10 shape is upgraded with the trusted-link table and receipt
/// identity columns. Legacy rows remain readable but cannot authorize a new
/// trusted Proposal because both new identity fields default to empty.
#[test]
fn migration_v10_to_v11_preserves_receipts_and_adds_trust_fields() -> Result<()> {
    let db_path = unique_temp_path();
    {
        let _journal = JournalStore::open(&db_path)?;
    }
    {
        let conn = Connection::open(&db_path)?;
        conn.execute(
            "INSERT INTO hcr_receipt_identities
             (hcr_id,claim_id,run_id,idempotency_key,payload_digest,receipt_event_id,
              harness_execution_id,overall_outcome,candidate_digest,artifact_ref,
              artifact_digest,evidence_digest)
             VALUES ('legacy_hcr','legacy_claim','legacy_run','legacy_key','payload','event',
                     'execution','CandidatePassed',?1,?1,?1,?1)",
            ["sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
        )?;
        conn.execute_batch(
            "DROP TABLE capability_change_approvals;
             DROP TABLE capability_proposal_hcr_links;
             DROP TABLE coding_task_submissions;
             DROP TABLE component_registry_entries;
             DROP TABLE component_registry_state;
             DROP TABLE component_deployment_intents;
             DROP TABLE component_deployment_receipts;
             DROP TABLE component_control_intents;
             DROP TABLE component_control_receipts;
             DROP TABLE component_registry_snapshots;
             ALTER TABLE hcr_receipt_identities DROP COLUMN invocation_id;
             ALTER TABLE hcr_receipt_identities DROP COLUMN candidate_id;
             ALTER TABLE hcr_receipt_identities DROP COLUMN receipt_digest;
             ALTER TABLE hcr_receipt_identities DROP COLUMN opaque_payload_digest;
             PRAGMA user_version=10;",
        )?;
    }

    let journal = JournalStore::open(&db_path)?;
    assert_eq!(journal.schema_version()?, 14);
    let conn = Connection::open(&db_path)?;
    let link_table: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type='table' AND name='capability_proposal_hcr_links'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(link_table, 1);
    let submission_table: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type='table' AND name='coding_task_submissions'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(submission_table, 1);
    let legacy: (String, String) = conn.query_row(
        "SELECT candidate_id,invocation_id FROM hcr_receipt_identities
         WHERE hcr_id='legacy_hcr'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(legacy, (String::new(), String::new()));
    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// A genuine v11 schema with a trusted Proposal/link is upgraded without
/// rewriting either row, and receives a fully bound pending Approval.
#[test]
fn migration_v11_to_v12_preserves_and_backfills_trusted_proposal() -> Result<()> {
    let db_path = unique_temp_path();
    let candidate = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let artifact = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let manifest = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    let evidence = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    {
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))?;
        conn.execute_batch(include_str!("../migrations/0002_registry_snapshots.sql"))?;
        conn.execute_batch(include_str!(
            "../migrations/0003_external_harness_hotload.sql"
        ))?;
        conn.execute_batch(include_str!(
            "../migrations/0004_capability_change_proposals.sql"
        ))?;
        conn.execute_batch(include_str!(
            "../migrations/0005_remove_manifest_operation_name_unique.sql"
        ))?;
        conn.execute_batch(include_str!(
            "../migrations/0006_external_operation_grants.sql"
        ))?;
        conn.execute_batch(include_str!(
            "../migrations/0007_harness_change_requests.sql"
        ))?;
        conn.execute_batch(include_str!("../migrations/0008_hcr_claims.sql"))?;
        conn.execute_batch(include_str!("../migrations/0009_hcr_evidence.sql"))?;
        conn.execute_batch(include_str!("../migrations/0010_hcr_receipt_identity.sql"))?;
        conn.execute_batch(include_str!(
            "../migrations/0011_capability_proposal_hcr_links.sql"
        ))?;
        conn.pragma_update(None, "user_version", 11)?;
        conn.execute(
            "INSERT INTO capability_change_proposals
             (proposal_id,submitter_principal_id,target_agent_id,origin_session_id,
              origin_run_id,artifact_ref,artifact_digest,manifest_ref,manifest_digest,
              evidence_ref,evidence_digest,requested_operations_json,risk_summary,
              expected_active_snapshot_id,status,created_at,expires_at)
             VALUES ('proposal_v11','owner_v11','agent_v11','session_v11','run_v11',
                     ?1,?1,?2,?2,?3,?3,'[\"external.calculator\"]','bound risk',
                     'snapshot_v11','PendingApproval',
                     '2026-07-14T00:00:00+00:00','2026-08-14T00:00:00+00:00')",
            rusqlite::params![artifact, manifest, evidence],
        )?;
        conn.execute(
            "INSERT INTO capability_proposal_hcr_links
             (proposal_id,hcr_id,claim_id,run_id,operation,candidate_id,candidate_digest,
              artifact_ref,artifact_digest,evidence_digest,source_registry_snapshot_id,
              settlement_id,created_at)
             VALUES ('proposal_v11','hcr_v11','claim_v11','hcr_run_v11',
                     'external.calculator','candidate_v11',?1,?2,?2,?3,
                     'snapshot_v11','settlement_v11','2026-07-14T00:00:00+00:00')",
            rusqlite::params![candidate, artifact, evidence],
        )?;
    }

    let journal = JournalStore::open(&db_path)?;
    assert_eq!(journal.schema_version()?, 14);
    let approval = journal
        .load_capability_approval_by_proposal("proposal_v11")?
        .expect("trusted v11 proposal must receive an approval");
    assert_eq!(approval.proposal_id, "proposal_v11");
    assert_eq!(approval.owner_principal_id, "owner_v11");
    assert_eq!(approval.source_registry_snapshot_id, "snapshot_v11");
    assert_eq!(approval.candidate_digest, candidate);
    assert_eq!(approval.artifact_digest, artifact);
    assert_eq!(approval.manifest_digest, manifest);
    assert!(approval.decision_nonce.len() >= 32);
    assert_eq!(
        approval.status,
        agent_core_kernel::domain::CapabilityApprovalStatus::Pending
    );
    let replay = journal
        .load_approval_replay_identity(&approval.approval_id)?
        .expect("backfilled approval replay identity must load");
    assert_eq!(replay.decision_nonce, approval.decision_nonce);
    assert!(replay.decision_id.is_none());

    let conn = Connection::open(&db_path)?;
    let preserved: (String, String) = conn.query_row(
        "SELECT p.risk_summary,l.hcr_id
         FROM capability_change_proposals p
         JOIN capability_proposal_hcr_links l ON l.proposal_id=p.proposal_id
         WHERE p.proposal_id='proposal_v11'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(preserved, ("bound risk".into(), "hcr_v11".into()));
    let immutable = conn.execute(
        "UPDATE capability_change_approvals SET artifact_digest=?1 WHERE proposal_id='proposal_v11'",
        [candidate],
    );
    assert!(
        immutable.is_err(),
        "approval artifact binding must be immutable"
    );
    let incomplete_approval = conn.execute(
        "UPDATE capability_change_approvals
         SET status='Approved',decision_id='decision_bad_approved',
             decision_payload_digest=?1,decision_result_json='{}',
             decided_at='2026-07-14T00:01:00+00:00',decided_by='owner_v11'
         WHERE proposal_id='proposal_v11'",
        [candidate],
    );
    assert!(
        incomplete_approval.is_err(),
        "Approved must bind both the deployed host identity and published snapshot"
    );
    let failed_with_snapshot = conn.execute(
        "UPDATE capability_change_approvals
         SET status='ActivationFailed',decision_id='decision_bad_failure',
             decision_payload_digest=?1,decision_result_json='{}',
             decided_at='2026-07-14T00:01:00+00:00',decided_by='owner_v11',
             activated_snapshot_id='snapshot_must_not_publish',activation_error='host failed'
         WHERE proposal_id='proposal_v11'",
        [candidate],
    );
    assert!(
        failed_with_snapshot.is_err(),
        "ActivationFailed must never publish a snapshot"
    );

    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// A database newer than the kernel must be rejected at startup.
#[test]
fn newer_schema_version_is_rejected_cleanly() -> Result<()> {
    let db_path = unique_temp_path();
    // Pre-stamp as version 15 (newer than kernel's CURRENT_SCHEMA_VERSION of 14).
    {
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))?;
        conn.pragma_update(None, "user_version", 15)?;
    }
    // Opening with the kernel (whose CURRENT_SCHEMA_VERSION is 14) must fail.
    let message = match JournalStore::open(&db_path) {
        Ok(_) => panic!("a newer-than-supported schema version must be rejected at startup"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("newer than supported version"),
        "error must explain the version mismatch, got: {message}"
    );
    // Sanitized: the message must reference versions only, not paths/secrets.
    assert!(
        !message.contains(db_path.to_string_lossy().as_ref()),
        "error must not leak the db path"
    );

    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// A unique .db path directly under the OS temp dir (no wrapper dir, which
/// avoids SQLite's bundled "database file has moved" quirk on re-open).
fn unique_temp_path() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("agent-core-schema-{}-{}.db", std::process::id(), n))
}

#[test]
fn migration_v3_to_v4_creates_proposals_table() -> Result<()> {
    let db_path = unique_temp_path();
    // Create a v3 database.
    {
        JournalStore::open(&db_path)?;
    }
    // Verify version is 8 and proposals table exists.
    {
        let journal = JournalStore::open(&db_path)?;
        assert_eq!(journal.schema_version()?, 14);
        let conn = rusqlite::Connection::open(&db_path)?;
        let has_table: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='capability_change_proposals'",
            [], |row| row.get(0),
        )?;
        assert!(
            has_table,
            "capability_change_proposals table must exist after v3→v9 migration"
        );
    }
    // Verify we can INSERT and SELECT.
    {
        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute(
            "INSERT INTO capability_change_proposals (proposal_id, submitter_principal_id, target_agent_id, origin_session_id, origin_run_id, artifact_ref, artifact_digest, manifest_ref, manifest_digest, evidence_ref, evidence_digest, requested_operations_json, risk_summary, expected_active_snapshot_id, status, created_at, expires_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            rusqlite::params!["test_prop", "submitter", "agent", "sess", "run", "ref", "sha256:0000000000000000000000000000000000000000000000000000000000000000", "mref", "sha256:1111111111111111111111111111111111111111111111111111111111111111", "eref", "sha256:2222222222222222222222222222222222222222222222222222222222222222", "[\"test.op\"]", "risk", "snap_0", "PendingApproval", "2026-01-01T00:00:00Z", "2026-02-01T00:00:00Z"],
        )?;
    }
    std::fs::remove_file(&db_path).ok();
    Ok(())
}

// ── Migration 0005: remove UNIQUE(operation_name) from harness_manifests ────

/// Build a v4 database (migrations 0001–0004 applied, version stamped 4),
/// inserting a harness_manifests row under the OLD UNIQUE(operation_name)
/// constraint, then reopen with the kernel to drive the v4→v5 migration.
fn build_v4_db_with_manifest(db_path: &std::path::Path) -> anyhow::Result<()> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch(include_str!("../migrations/0001_init.sql"))?;
    conn.execute_batch(include_str!("../migrations/0002_registry_snapshots.sql"))?;
    conn.execute_batch(include_str!(
        "../migrations/0003_external_harness_hotload.sql"
    ))?;
    conn.execute_batch(include_str!(
        "../migrations/0004_capability_change_proposals.sql"
    ))?;
    // Stamp at v4 (pre-0005).
    conn.pragma_update(None, "user_version", 4)?;
    // Insert a manifest row under the v4 schema (UNIQUE(operation_name) present).
    conn.execute(
        "INSERT INTO harness_manifests
         (manifest_id, harness_id, artifact_digest, protocol_version, endpoint,
          operation_name, description, input_schema_json, output_schema_json,
          idempotent, created_at, canonical_digest)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        rusqlite::params![
            "manifest_pre_existing",
            "h",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "external-harness-v1",
            "http://127.0.0.1:7000/x",
            "external.pre_existing",
            "pre-existing manifest",
            "{}",
            "{}",
            1,
            "2026-01-01T00:00:00Z",
            "canonical_pre_existing",
        ],
    )?;
    Ok(())
}

/// A v4 database with pre-existing manifest data migrates cleanly to v8,
/// preserving the row, removing UNIQUE(operation_name), adding the
/// operation_name index, allowing two manifest_ids for one operation_name,
/// creating the external_operation_grants table, and creating the
/// harness_change_requests table (plus hcr_claims and run mode column).
#[test]
fn migration_v4_to_v5_preserves_data_and_drops_unique_constraint() -> Result<()> {
    let db_path = unique_temp_path();
    build_v4_db_with_manifest(&db_path)?;

    // Reopen with the kernel → drives v4→v5→v6→v7→v8→v9 migration.
    let journal = JournalStore::open(&db_path)?;
    // Schema version is current after the complete migration chain.
    assert_eq!(journal.schema_version()?, 14);

    let conn = Connection::open(&db_path)?;

    // 2. Pre-existing manifest data is preserved.
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM harness_manifests WHERE manifest_id = 'manifest_pre_existing'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1, "pre-existing manifest row must survive migration");

    // 3. The same operation_name can now hold a DIFFERENT manifest_id
    //    (UNIQUE(operation_name) is gone).
    conn.execute(
        "INSERT INTO harness_manifests
         (manifest_id, harness_id, artifact_digest, protocol_version, endpoint,
          operation_name, description, input_schema_json, output_schema_json,
          idempotent, created_at, canonical_digest)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        rusqlite::params![
            "manifest_v2",
            "h",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "external-harness-v1",
            "http://127.0.0.1:7000/x",
            "external.pre_existing",
            "upgraded manifest",
            "{}",
            "{}",
            1,
            "2026-01-02T00:00:00Z",
            "canonical_v2",
        ],
    )
    .expect("inserting a second manifest_id for the same operation_name must succeed after 0005");

    // 4. operation_name index exists.
    let idx_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_harness_manifests_operation_name'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        idx_count, 1,
        "idx_harness_manifests_operation_name must exist"
    );

    // 5. The old UNIQUE(operation_name) constraint is gone: confirm via the
    //    table's CREATE SQL — it must NOT mention UNIQUE(operation_name). The
    //    only UNIQUE on operation_name in v4 was a table-level constraint; the
    //    recreated table must not carry it.
    let create_sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='harness_manifests'",
        [],
        |row| row.get(0),
    )?;
    assert!(
        !create_sql.contains("UNIQUE (operation_name)"),
        "UNIQUE(operation_name) must be removed, got: {create_sql}"
    );

    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// Re-opening a fully migrated database is a no-op and no duplicate indexes
/// are created.
#[test]
fn migration_v5_is_idempotent_on_reopen() -> Result<()> {
    let db_path = unique_temp_path();
    // First open: migrates to v8.
    {
        let _journal = JournalStore::open(&db_path)?;
        let conn = Connection::open(&db_path)?;
        let v: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(v, 14);
    }
    // Second open: must be a no-op.
    let journal = JournalStore::open(&db_path)?;
    assert_eq!(journal.schema_version()?, 14);

    let conn = Connection::open(&db_path)?;
    // The operation_name index is created with IF NOT EXISTS, so no duplicate.
    let idx_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_harness_manifests_operation_name'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(idx_count, 1, "index must not be duplicated on reopen");

    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// Migration v5→v6 creates the external_operation_grants table and its indexes.
#[test]
fn migration_v5_to_v6_creates_grants_table() -> Result<()> {
    let db_path = unique_temp_path();
    // First open: migrates to v8 via fresh DB.
    {
        let _journal = JournalStore::open(&db_path)?;
        let conn = Connection::open(&db_path)?;
        let v: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(v, 14);
    }
    // Reopen: idempotent.
    {
        let journal = JournalStore::open(&db_path)?;
        assert_eq!(journal.schema_version()?, 14);
        let conn = Connection::open(&db_path)?;

        // Table exists.
        let has_table: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='external_operation_grants'",
            [],
            |row| row.get(0),
        )?;
        assert!(
            has_table,
            "external_operation_grants table must exist after v5→v6 migration"
        );

        // Indexes exist.
        let idx_lookup: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_ext_op_grants_lookup'",
            [],
            |row| row.get(0),
        )?;
        assert!(idx_lookup, "idx_ext_op_grants_lookup must exist");

        let idx_snapshot: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_ext_op_grants_snapshot'",
            [],
            |row| row.get(0),
        )?;
        assert!(idx_snapshot, "idx_ext_op_grants_snapshot must exist");

        // Can INSERT and SELECT.
        conn.execute(
            "INSERT INTO external_operation_grants
             (grant_id, operation, grantee_principal_id, channel, conversation_kind,
              scope, risk, snapshot_id, status, created_at)
             VALUES ('grt_test_1', 'external.calculator', 'owner', 'Feishu', 'p2p',
                     'principal_channel', 'Write', 'snap_1', 'active', '2026-01-01T00:00:00Z')",
            [],
        )?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM external_operation_grants WHERE grant_id = 'grt_test_1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1, "inserted grant row must be queryable");
    }
    std::fs::remove_file(&db_path).ok();
    Ok(())
}

/// Migration v6→v9 creates the harness_change_requests table, hcr_claims, and run mode column.
#[test]
fn migration_v6_to_v8_creates_hcr_tables() -> Result<()> {
    let db_path = unique_temp_path();
    // First open: migrates to v8 via fresh DB.
    {
        let _journal = JournalStore::open(&db_path)?;
        let conn = Connection::open(&db_path)?;
        let v: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(v, 14);
    }
    // Reopen: idempotent.
    {
        let journal = JournalStore::open(&db_path)?;
        assert_eq!(journal.schema_version()?, 14);
        let conn = Connection::open(&db_path)?;

        // Table exists.
        let has_table: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='harness_change_requests'",
            [],
            |row| row.get(0),
        )?;
        assert!(
            has_table,
            "harness_change_requests table must exist after v6→v7 migration"
        );

        // Indexes exist.
        let idx_dedup: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_hcr_source_dedup'",
            [],
            |row| row.get(0),
        )?;
        assert!(idx_dedup, "idx_hcr_source_dedup must exist");

        let idx_status: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_hcr_status'",
            [],
            |row| row.get(0),
        )?;
        assert!(idx_status, "idx_hcr_status must exist");

        // Can INSERT and SELECT.
        conn.execute(
            "INSERT INTO harness_change_requests
             (request_id, source, source_message_id, session_id, principal_id,
              channel, chat_type, harness_id, requirement, status, created_at, updated_at)
             VALUES ('hcr_test_1', 'Feishu', 'om_test_msg', 'sess_1', 'principal_1',
                     'Feishu', 'p2p', 'my-harness', 'test requirement',
                     'pending', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM harness_change_requests WHERE request_id = 'hcr_test_1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1, "inserted HCR row must be queryable");

        // Unique constraint works: duplicate (source, source_message_id) is rejected.
        let dup_result = conn.execute(
            "INSERT INTO harness_change_requests
             (request_id, source, source_message_id, session_id, principal_id,
              channel, chat_type, harness_id, requirement, status, created_at, updated_at)
             VALUES ('hcr_test_2', 'Feishu', 'om_test_msg', 'sess_1', 'principal_1',
                     'Feishu', 'p2p', 'my-harness', 'duplicate',
                     'pending', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        );
        assert!(
            dup_result.is_err(),
            "duplicate (source, source_message_id) must be rejected"
        );
    }
    std::fs::remove_file(&db_path).ok();
    Ok(())
}
