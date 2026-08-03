-- Preserve every Coding Harness submission attempt while allowing a new
-- attempt only after the Harness has definitively rejected the previous one.
--
-- Legacy `failed` rows did not distinguish a rejection from a transport or
-- timeout failure.  They are therefore migrated to `outcome_unknown` so the
-- Kernel continues to fail closed after upgrade.

ALTER TABLE coding_task_submissions RENAME TO coding_task_submissions_v20;

-- v21-stage-boundary
CREATE TABLE coding_task_submissions (
    attempt_id          TEXT NOT NULL PRIMARY KEY CHECK(length(attempt_id) > 0),
    source_message_id   TEXT NOT NULL CHECK(length(source_message_id) > 0),
    attempt_sequence    INTEGER NOT NULL CHECK(attempt_sequence > 0),
    submission_call_key TEXT NOT NULL UNIQUE CHECK(length(submission_call_key) > 0),
    request_digest      TEXT NOT NULL CHECK(length(request_digest) = 71),
    invocation_id       TEXT NOT NULL UNIQUE CHECK(length(invocation_id) > 0),
    origin_run_id       TEXT NOT NULL CHECK(length(origin_run_id) > 0),
    origin_session_id   TEXT NOT NULL CHECK(length(origin_session_id) > 0),
    status              TEXT NOT NULL CHECK(status IN (
                            'running',
                            'succeeded',
                            'definitively_rejected',
                            'outcome_unknown'
                        )),
    result_json         TEXT,
    error_code          TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE(source_message_id, attempt_sequence)
) STRICT;

-- v21-stage-boundary
INSERT INTO coding_task_submissions (
    attempt_id, source_message_id, attempt_sequence, submission_call_key,
    request_digest, invocation_id, origin_run_id, origin_session_id,
    status, result_json, error_code, created_at, updated_at
)
SELECT
    invocation_id,
    source_message_id,
    1,
    'legacy:' || invocation_id,
    request_digest,
    invocation_id,
    origin_run_id,
    origin_session_id,
    CASE status
        WHEN 'running' THEN 'running'
        WHEN 'succeeded' THEN 'succeeded'
        ELSE 'outcome_unknown'
    END,
    result_json,
    CASE status
        WHEN 'failed' THEN COALESCE(error_code, 'LEGACY_OUTCOME_UNKNOWN')
        ELSE error_code
    END,
    created_at,
    updated_at
FROM coding_task_submissions_v20;

-- v21-stage-boundary
DROP TABLE coding_task_submissions_v20;

-- v21-stage-boundary
CREATE INDEX idx_coding_task_submissions_message_sequence
    ON coding_task_submissions(source_message_id, attempt_sequence DESC);
