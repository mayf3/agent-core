//! Fixed North Star invocation router.
//!
//! This router recognises only the approved calculator smoke request.  It is
//! intentionally not a general expression parser and never evaluates the
//! expression itself.

use crate::domain::{RuntimeEventPayload, ValidatedEvent};

pub const CALCULATOR_SMOKE_SENTENCE: &str = "用 external.calculator 计算 6 * 7";

pub fn matches(event: &ValidatedEvent) -> bool {
    let RuntimeEventPayload::UserMessage { text, .. } = &event.payload;
    text.trim() == CALCULATOR_SMOKE_SENTENCE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentence_is_fixed_and_not_a_general_expression_parser() {
        assert_eq!(
            CALCULATOR_SMOKE_SENTENCE,
            "用 external.calculator 计算 6 * 7"
        );
        assert_ne!(
            CALCULATOR_SMOKE_SENTENCE,
            "用 external.calculator 计算 7 * 6"
        );
    }
}
