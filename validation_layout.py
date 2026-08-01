#!/usr/bin/env python3
"""Validation script for File-backed Agent Binding V0 deliverables.

Checks that every file listed in the delivery plan exists and satisfies basic
expectations, and that no unexpected files were added under the new
`bindings/` output directory.
"""
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent

REQUIRED_FILES = {
    "bindings/feishu.json": None,
    "bindings/feishu.schema.json": None,
    "src/binding.rs": None,
    "src/gateway/tests.rs": None,
    "src/lib.rs": lambda p: "pub mod binding;" in p.read_text(),
    "src/domain/context_block.rs": lambda p: "WorkspaceRoot" in p.read_text(),
    "src/gateway/mod.rs": lambda p: "feishu_agent_id" in p.read_text()
    and '"agent_id"' in p.read_text(),
    "src/context.rs": lambda p: "WorkspaceRoot" in p.read_text()
    and "session.agent_id" in p.read_text(),
}

# Only these files may live under the new bindings/ output directory.
ALLOWED_BINDINGS_FILES = {"feishu.json", "feishu.schema.json"}

FAILURES = []


def main() -> int:
    for rel, check in sorted(REQUIRED_FILES.items()):
        path = ROOT / rel
        if not path.exists():
            FAILURES.append(f"MISSING: {rel}")
            continue
        if check is not None:
            try:
                ok = check(path)
            except Exception as exc:  # noqa: BLE001
                FAILURES.append(f"CHECK ERROR: {rel}: {exc}")
                continue
            if not ok:
                FAILURES.append(f"CONTENT MISSING: {rel}")

    bindings_dir = ROOT / "bindings"
    if bindings_dir.exists():
        for child in bindings_dir.iterdir():
            if child.name not in ALLOWED_BINDINGS_FILES:
                FAILURES.append(f"UNEXPECTED FILE: bindings/{child.name}")

    example = ROOT / "bindings" / "feishu.json"
    if example.exists():
        try:
            data = json.loads(example.read_text())
            assert data.get("version") == 1, "version must be 1"
            assert isinstance(data.get("bindings"), list), "bindings must be an array"
            for b in data["bindings"]:
                assert isinstance(b.get("chat_id"), str) and b["chat_id"], "chat_id required"
                assert isinstance(b.get("agent_id"), str) and b["agent_id"], "agent_id required"
        except (json.JSONDecodeError, AssertionError, TypeError) as exc:
            FAILURES.append(f"INVALID EXAMPLE JSON: {exc}")

    if FAILURES:
        print("VALIDATION FAILED:")
        for failure in FAILURES:
            print(f"  - {failure}")
        return 1
    print("VALIDATION OK: all deliverables present and consistent")
    return 0


if __name__ == "__main__":
    sys.exit(main())
