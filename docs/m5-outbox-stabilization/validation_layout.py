#!/usr/bin/env python3
"""
M5 Outbox Stabilization — layout validation.

Checks the deliverable files exist and contain expected anchors. Does NOT
execute cargo or pnpm. Run after every stage to catch accidental deletions or
unexpected new files.

Usage:
    python3 docs/m5-outbox-stabilization/validation_layout.py
"""

from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def must_contain(rel_path: str, needle: str) -> None:
    path = ROOT / rel_path
    assert path.exists(), f"missing: {rel_path}"
    text = path.read_text(encoding="utf-8")
    assert needle in text, f"{rel_path}: missing anchor '{needle}'"


def must_not_contain(rel_path: str, needle: str) -> None:
    path = ROOT / rel_path
    assert path.exists(), f"missing: {rel_path}"
    text = path.read_text(encoding="utf-8")
    assert needle not in text, f"{rel_path}: forbidden anchor '{needle}' still present"


def must_exist(rel_path: str) -> None:
    path = ROOT / rel_path
    assert path.exists(), f"missing: {rel_path}"


def must_be_absent(rel_path: str) -> None:
    path = ROOT / rel_path
    assert not path.exists(), f"unexpected file present: {rel_path}"


def main() -> int:
    # Source deliverables
    must_exist("src/domain/mod.rs")
    must_exist("src/domain/status.rs")
    must_exist("src/domain/retry.rs")
    must_exist("src/journal/sqlite.rs")
    must_exist("src/journal/outbox_queue.rs")
    must_exist("src/journal/unknown.rs")
    must_exist("src/journal/queue_health.rs")
    must_exist("src/journal/outbox.rs")
    must_exist("src/server/mod.rs")
    must_exist("src/server/delivery.rs")
    must_exist("src/server/delivery_tests.rs")
    must_exist("src/server/dispatcher_metrics.rs")
    must_exist("src/runtime/outbox_dispatcher.rs")

    # Anchor: RunFailed event kind
    must_contain("src/domain/mod.rs", "RunFailed,")
    must_contain("src/journal/sqlite.rs", '"RunFailed" => JournalEventKind::RunFailed')

    # Anchor: succeed/fail helpers write Run transitions
    must_contain(
        "src/journal/outbox_queue.rs",
        "JournalEventKind::RunCompleted",
    )
    must_contain(
        "src/journal/outbox_queue.rs",
        "JournalEventKind::RunFailed",
    )

    # Anchor: recovery writes OutboxDispatchUnknown, not ReceiptReceived(Unknown)
    must_contain("src/journal/unknown.rs", "JournalEventKind::OutboxDispatchUnknown")
    must_not_contain(
        "src/journal/unknown.rs",
        '"status": "Unknown"',
    )

    # Anchor: health snapshot exposes dispatcher + per-status counts
    must_contain(
        "src/server/mod.rs",
        "outbox_dispatcher_enabled",
    )
    must_contain(
        "src/server/mod.rs",
        "outbox_pending_count",
    )
    must_contain(
        "src/server/mod.rs",
        "outbox_unknown_count",
    )
    must_contain(
        "src/server/mod.rs",
        "outbox_dispatching_count",
    )

    # Anchor: dispatcher observability fields (HANDOVER §4.4)
    must_contain("src/server/mod.rs", "outbox_dispatcher_running")
    must_contain("src/server/mod.rs", "last_dispatch_tick_at")
    must_contain("src/server/mod.rs", "last_dispatch_error_category")
    must_contain("src/server/dispatcher_metrics.rs", "pub struct DispatcherMetrics")
    must_contain("src/server/dispatcher_metrics.rs", "pub struct LoopGuard")

    # Anchor: queue_health exposes single-status counter
    must_contain(
        "src/journal/queue_health.rs",
        "pub fn outbox_status_count",
    )

    # Anchor: dispatcher startup log
    must_contain(
        "src/server/mod.rs",
        "existing_pending_outbox_count",
    )
    must_contain(
        "src/server/mod.rs",
        "unknown items will not be retried automatically",
    )

    # Negative anchor: TS connector untouched (still has its single execute route)
    must_exist("connectors/feishu/src/execute-server.ts")

    # Doc anchors
    must_contain(
        "docs/architecture/outbox.md",
        "OutboxDispatchUnknown",
    )
    must_not_contain(
        "docs/architecture/outbox.md",
        "Runtime still sends approved invocations synchronously",
    )

    # Tests should still compile-ready
    must_exist("tests/m0_kernel.rs")
    must_exist("tests/m5_queue_projection.rs")

    # Forbidden new files
    must_be_absent("src/runtime/sync_dispatch.rs")
    must_be_absent("connectors/feishu/src/reaction_retry.ts")
    must_be_absent("scripts/deploy-m5.sh")

    print("validation_layout: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
