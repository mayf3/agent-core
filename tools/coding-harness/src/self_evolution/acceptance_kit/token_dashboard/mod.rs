//! Token Dashboard Acceptance Kit.
//!
//! Re-exports from public_spec and private_verifier modules.
//! Also defines private verification cases — hidden inputs from which
//! expected values are computed at verification time.

mod private_verifier;
mod public_spec;

use crate::self_evolution::acceptance_kit::PrivateVerificationCase;

pub use private_verifier::verify;
pub use public_spec::public_spec;

/// Private verification cases for Token Dashboard.
///
/// Each case contains a different distribution of invocation events so that
/// the verifier must compute expectations from the actual input rather than
/// matching a single fixed pattern. Evaluation times are frozen so that
/// rolling-window results are date-independent.
///
/// Case A (standard): mixed completed + failed + missing tokens + unknown event
/// Case B (alternate): different model/profile/run/date/latency distribution,
///                     zero failures, edge case for window boundaries.
pub(super) fn private_verification_cases() -> &'static [PrivateVerificationCase] {
    &[
        PrivateVerificationCase {
            case_id: "token-case-A",
            evaluation_time_utc: "2026-07-18T00:00:00Z",
            input: r#"{"schema_version":"event.observe.v0","next_cursor":4,"has_more":false,"events":[
                {"event_id":"c1","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-15T10:00:00Z","run_id":"run-alpha","payload":{"model":"gpt-4","profile":"research","provider":"openai","latency_ms":120,"input_tokens":50,"cached_input_tokens":10,"output_tokens":30,"reasoning_tokens":5,"total_tokens":95}},
                {"event_id":"c2","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-16T14:30:00Z","run_id":"run-alpha","payload":{"model":"gpt-4","profile":"research","provider":"openai","latency_ms":200,"input_tokens":100,"cached_input_tokens":0,"output_tokens":60,"reasoning_tokens":10,"total_tokens":170}},
                {"event_id":"f1","event_kind":"model.invocation.failed.v0","occurred_at":"2026-07-15T11:00:00Z","run_id":"run-beta","payload":{"model":"claude-3","profile":"production","provider":"anthropic","latency_ms":5000,"error_category":"rate_limited"}},
                {"event_id":"c3","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-17T09:00:00Z","run_id":"run-alpha","payload":{"model":"gpt-4","profile":"research","provider":"openai","latency_ms":null,"input_tokens":null,"cached_input_tokens":null,"output_tokens":null,"reasoning_tokens":null,"total_tokens":null}},
                {"event_id":"unk-1","event_kind":"future.observed.v99","occurred_at":"2026-07-18T00:00:00Z","payload":{"unknown":true}}
            ]}"#,
        },
        PrivateVerificationCase {
            case_id: "token-case-B",
            evaluation_time_utc: "2026-07-18T00:00:00Z",
            input: r#"{"schema_version":"event.observe.v0","next_cursor":3,"has_more":false,"events":[
                {"event_id":"c4","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-10T08:00:00Z","run_id":"run-delta","payload":{"model":"llama-3","profile":"eval","provider":"meta","latency_ms":85,"input_tokens":200,"cached_input_tokens":50,"output_tokens":40,"reasoning_tokens":0,"total_tokens":290}},
                {"event_id":"c5","event_kind":"model.invocation.completed.v0","occurred_at":"2026-07-12T16:00:00Z","run_id":"run-delta","payload":{"model":"llama-3","profile":"eval","provider":"meta","latency_ms":150,"input_tokens":500,"cached_input_tokens":100,"output_tokens":120,"reasoning_tokens":0,"total_tokens":720}},
                {"event_id":"unk-2","event_kind":"unknown.future.v1","occurred_at":"2026-07-18T00:00:00Z","payload":{"signal":"ignore"}}
            ]}"#,
        },
    ]
}
