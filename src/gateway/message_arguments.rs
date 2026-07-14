use anyhow::{bail, Result};
use serde_json::Value;

const PENDING_PROPOSAL_KIND: &str = "capability_proposal_pending_v1";

/// Validate the Kernel-owned argument contract for Feishu replies.
///
/// A reply has a fixed destination and exactly one user-visible mode: plain
/// text or the narrowly-scoped pending Proposal presentation.  The latter is
/// deliberately not an arbitrary Connector card payload; the Connector must
/// reload all card fields from the authoritative Proposal API.
pub(super) fn validate_feishu_send_arguments(arguments: &Value) -> Result<()> {
    super::string_arg(arguments, "message_id")?;
    super::string_arg(arguments, "chat_id")?;

    let has_text = arguments
        .get("text")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty());
    let presentation = arguments.get("presentation");
    let has_presentation = match presentation {
        None => false,
        Some(value) => {
            validate_pending_proposal_presentation(value)?;
            true
        }
    };

    if has_text == has_presentation {
        bail!("feishu send requires exactly one of text or presentation");
    }
    if arguments.get("text").is_some() && !has_text {
        bail!("feishu send text must be a non-empty string");
    }
    Ok(())
}

fn validate_pending_proposal_presentation(value: &Value) -> Result<()> {
    let presentation = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("invalid feishu presentation"))?;
    if presentation.len() != 2
        || presentation.get("kind").and_then(Value::as_str) != Some(PENDING_PROPOSAL_KIND)
        || !presentation
            .get("proposal_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.trim().is_empty())
    {
        bail!("invalid feishu presentation");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn destination() -> Value {
        json!({"message_id":"om_1", "chat_id":"oc_1"})
    }

    #[test]
    fn accepts_plain_text_or_fixed_pending_proposal_presentation() {
        let mut text = destination();
        text["text"] = json!("hello");
        assert!(validate_feishu_send_arguments(&text).is_ok());

        let mut card = destination();
        card["presentation"] = json!({
            "kind": PENDING_PROPOSAL_KIND,
            "proposal_id": "proposal_1",
        });
        assert!(validate_feishu_send_arguments(&card).is_ok());
    }

    #[test]
    fn rejects_missing_or_ambiguous_presentation_mode() {
        assert!(validate_feishu_send_arguments(&destination()).is_err());

        let mut both = destination();
        both["text"] = json!("fallback");
        both["presentation"] = json!({
            "kind": PENDING_PROPOSAL_KIND,
            "proposal_id": "proposal_1",
        });
        assert!(validate_feishu_send_arguments(&both).is_err());
    }

    #[test]
    fn rejects_empty_text_and_malformed_or_extensible_presentations() {
        for text in [json!(""), json!("   "), json!(7)] {
            let mut arguments = destination();
            arguments["text"] = text;
            assert!(validate_feishu_send_arguments(&arguments).is_err());
        }

        for presentation in [
            json!({"kind":"other", "proposal_id":"proposal_1"}),
            json!({"kind":PENDING_PROPOSAL_KIND, "proposal_id":""}),
            json!({"kind":PENDING_PROPOSAL_KIND, "proposal_id":"proposal_1", "card":{}}),
            json!("proposal_1"),
        ] {
            let mut arguments = destination();
            arguments["presentation"] = presentation;
            assert!(validate_feishu_send_arguments(&arguments).is_err());
        }
    }

    #[test]
    fn requires_non_empty_message_and_chat_destinations() {
        for (message_id, chat_id) in [("", "oc_1"), ("om_1", " ")] {
            let arguments = json!({
                "message_id": message_id,
                "chat_id": chat_id,
                "text": "hello",
            });
            assert!(validate_feishu_send_arguments(&arguments).is_err());
        }
    }
}
