//! Token Dashboard public specification.
//!
//! This is shown to the model during generation and repair as part of
//! the per-request context (not the SYSTEM_PROMPT).

use serde_json::{json, Value};

/// Public specification for the Token Dashboard kit.
pub fn public_spec() -> Value {
    json!({
        "kit_id": "token-dashboard-v0",
        "kit_version": "v0",
        "target_profile": "hook-consumer-service-v0",
        "description": "Token Dashboard — visualize model token usage, call counts, latency, failures, and availability across 1/7/30-day rolling windows, grouped by model, profile, and run.",

        "input_contract": {
            "contract_id": "event.observe.v0",
            "event_types": [
                "model.invocation.completed.v0",
                "model.invocation.failed.v0"
            ],
            "allowed_fields": {
                "event_id": "string — unique event identifier",
                "event_kind": "string — the kind of invocation event",
                "occurred_at": "RFC 3339 timestamp — when the event occurred",
                "run_id": "string — the run identifier, directly on the event envelope (not nested in payload)",
                "payload.profile": "string — the user/profile identifier for this invocation, may be absent",
                "payload.provider": "string — the LLM provider used",
                "payload.model": "string — the model identifier",
                "payload.latency_ms": "integer — invocation latency in milliseconds, present for completed and failed events",
                "payload.input_tokens": "nullable integer — input token count, may be null or absent",
                "payload.cached_input_tokens": "nullable integer — cached input token count, may be null or absent",
                "payload.output_tokens": "nullable integer — output token count, may be null or absent",
                "payload.reasoning_tokens": "nullable integer — reasoning token count, may be null or absent",
                "payload.total_tokens": "nullable integer — total token count, may be null or absent",
                "payload.error_category": "string — error category for failed invocations"
            },
            "missing_field_handling": "When a token field is null or absent, count it as unavailable (positive unavailable counter) rather than treating it as zero. Use a separate positive counter for unavailable token fields.",
            "time_format": "RFC 3339 in UTC (e.g. 2026-07-15T10:00:00Z)",
            "token_numeric_type": "u64 (non-negative integer)"
        },

        "output_json_schema": {
            "type": "object",
            "required": ["rolling_windows"],
            "properties": {
                "rolling_windows": {
                    "type": "object",
                    "description": "Container for 1-day, 7-day, and 30-day rolling windows",
                    "properties": {
                        "1_day": {"$ref": "#/definitions/window_set"},
                        "7_day": {"$ref": "#/definitions/window_set"},
                        "30_day": {"$ref": "#/definitions/window_set"}
                    }
                }
            },
            "definitions": {
                "window_set": {
                    "type": "object",
                    "description": "Aggregated metrics for one window size",
                    "properties": {
                        "overall": {"$ref": "#/definitions/window_metrics"},
                        "by_model": {
                            "type": "object",
                            "description": "Metrics keyed by model identifier",
                            "additionalProperties": {"$ref": "#/definitions/window_metrics"}
                        },
                        "by_profile": {
                            "type": "object",
                            "description": "Metrics keyed by profile identifier",
                            "additionalProperties": {"$ref": "#/definitions/window_metrics"}
                        }
                    }
                },
                "window_metrics": {
                    "type": "object",
                    "properties": {
                        "calls": {"type": "integer", "description": "Total invocations (completed + failed) in the window"},
                        "input_tokens": {"type": "integer", "description": "Total input tokens"},
                        "cached_tokens": {"type": "integer", "description": "Total cached input tokens"},
                        "output_tokens": {"type": "integer", "description": "Total output tokens"},
                        "reasoning_tokens": {"type": "integer", "description": "Total reasoning tokens"},
                        "total_tokens": {"type": "integer", "description": "Total tokens (including cached)"},
                        "avg_latency_ms": {"type": "number", "description": "Average latency across all invocations with latency_ms present"},
                        "failures": {"type": "integer", "description": "Count of failed invocations"},
                        "unavailable": {"type": "integer", "description": "Count of missing token fields across invocations"}
                    }
                }
            }
        },

        "aggregation_semantics": {
            "window_boundary": "1_day covers today (based on runtime today_utc), 7_day covers today + 6 prior days, 30_day covers today + 29 prior days. Use within_days helper with event_date.",
            "date_attribution": "Events are attributed to the date in their occurred_at field, not the processing date.",
            "success_and_failure_counting": "Both completed and failed invocations count toward calls and avg_latency_ms when latency_ms is present. Failures are additionally counted in the failures field.",
            "average_latency_computation": "Sum of all latency_ms values divided by count of invocations with non-null latency_ms. Use integer arithmetic; display as decimal if needed.",
            "missing_token_handling": "Any null or absent token field increments the unavailable counter for that invocation. Do NOT estimate missing values as zero.",
            "profile_missing_grouping": "When profile is absent from the event payload, use the string 'default' as the profile identifier.",
            "cached_tokens_in_total": "cached_tokens IS included in total_tokens (total_tokens = input_tokens + cached_tokens + output_tokens + reasoning_tokens).",
            "total_token_aggregation": "total_tokens is aggregated from per-invocation total_tokens values, not recomputed from sub-totals."
        },

        "html_contract": {
            "required_display": [
                "Date of the data",
                "Run identifiers",
                "Model identifiers",
                "Profile identifiers",
                "Token breakdown (input, cached, output, reasoning, total)",
                "Call count and average latency per window and dimension",
                "Failure counts",
                "Unavailable token counters"
            ],
            "behavior": "read_only",
            "prohibited": [
                "No mutation or modification requests",
                "No scripts or external assets",
                "No forms that would POST data"
            ],
            "style_requirement": "The page must be readable. Visual style is not prescribed — any plain HTML table or list is acceptable. All event-derived text must be HTML-escaped."
        },

        "examples": [
            {
                "description": "Example input with two completed events and one failed event",
                "input": {
                    "events": [
                        {"event_id": "e1", "event_kind": "model.invocation.completed.v0", "occurred_at": "2026-07-15T10:00:00Z", "run_id": "run-1", "payload": {"profile": "default", "provider": "test", "model": "model-a", "latency_ms": 20, "input_tokens": 10, "cached_input_tokens": 2, "output_tokens": 5, "reasoning_tokens": 1, "total_tokens": 16}},
                        {"event_id": "e2", "event_kind": "model.invocation.completed.v0", "occurred_at": "2026-07-15T11:00:00Z", "run_id": "run-2", "payload": {"profile": "analysis", "provider": "test", "model": "model-b", "latency_ms": 30, "input_tokens": 20, "cached_input_tokens": null, "output_tokens": 8, "reasoning_tokens": 2, "total_tokens": 30}},
                        {"event_id": "e3", "event_kind": "model.invocation.failed.v0", "occurred_at": "2026-07-15T12:00:00Z", "run_id": "run-1", "payload": {"profile": "default", "provider": "test", "model": "model-a", "latency_ms": null, "error_category": "rate_limited"}}
                    ]
                },
                "output_hint": "The 1_day overall window would have calls=3 (two completed + one failed), avg_latency_ms=25 (50ms from e1+e2, e3 null latency excluded), input_tokens=30, failures=1, unavailable>=1 (e2 has null cached_input_tokens). These are illustrative — your implementation must compute values from real events, not hardcode example values."
            }
        ],

        "notes": {
            "field_name_stability": "All field names in the output JSON schema are stable and required. Do not rename or omit fields.",
            "map_key_rules": "Map keys for by_model and by_profile are the actual model/profile identifiers from the events. Use 'unknown' for missing model values. Use 'default' for missing profile values.",
            "sorting_and_determinism": "Output map keys should be sorted lexicographically for deterministic JSON output.",
            "empty_result": "When there are no events, each rolling window should still be present with zeroed metrics (calls=0, all token totals=0, avg_latency_ms=0, failures=0, unavailable=0) and empty by_model/by_profile maps."
        }
    })
}
