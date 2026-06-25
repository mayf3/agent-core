#[cfg(test)]
mod config_wiring_tests {
    use crate::config::KernelConfig;
    use crate::llm::{OpenAiCompatibleLlm, ToolNameMode};

    fn cfg() -> KernelConfig {
        super::super::grant_schema_tests::_cfg()
    }

    fn wired_cfg() -> KernelConfig {
        let mut c = cfg();
        c.openai_base_url = "http://primary/v1".into();
        c.openai_api_key = "k".into();
        c.model = "m".into();
        c.fallback_openai_base_url = "http://fallback/v1".into();
        c.fallback_openai_api_key = "fk".into();
        c.fallback_model = "fm".into();
        c
    }

    fn primary_indexed(llm: &OpenAiCompatibleLlm) -> bool {
        matches!(llm.primary.tool_name_mode, ToolNameMode::IndexedMapping(_))
    }

    fn fallback_indexed(llm: &OpenAiCompatibleLlm) -> bool {
        llm.fallback
            .as_ref()
            .map(|e| matches!(e.tool_name_mode, ToolNameMode::IndexedMapping(_)))
            .unwrap_or(false)
    }

    #[test]
    fn config_both_passthrough_default() {
        let mut c = wired_cfg();
        c.primary_tool_name_indexed = false;
        c.fallback_tool_name_indexed = false;
        let llm = crate::server::build_llm_from_config(&c);
        assert!(!primary_indexed(&llm), "primary passthrough");
        assert!(!fallback_indexed(&llm), "fallback passthrough");
    }

    #[test]
    fn config_primary_indexed_only() {
        let mut c = wired_cfg();
        c.primary_tool_name_indexed = true;
        c.fallback_tool_name_indexed = false;
        let llm = crate::server::build_llm_from_config(&c);
        assert!(primary_indexed(&llm), "primary indexed");
        assert!(!fallback_indexed(&llm), "fallback still passthrough");
    }

    #[test]
    fn config_fallback_indexed_only() {
        let mut c = wired_cfg();
        c.primary_tool_name_indexed = false;
        c.fallback_tool_name_indexed = true;
        let llm = crate::server::build_llm_from_config(&c);
        assert!(!primary_indexed(&llm), "primary still passthrough");
        assert!(fallback_indexed(&llm), "fallback indexed");
    }

    #[test]
    fn config_both_indexed_independent() {
        let mut c = wired_cfg();
        c.primary_tool_name_indexed = true;
        c.fallback_tool_name_indexed = true;
        let llm = crate::server::build_llm_from_config(&c);
        assert!(primary_indexed(&llm));
        assert!(fallback_indexed(&llm));
    }

    #[test]
    fn config_fallback_indexed_without_endpoint_does_not_create_one() {
        let mut c = wired_cfg();
        c.fallback_openai_base_url = String::new();
        c.fallback_tool_name_indexed = true;
        let llm = crate::server::build_llm_from_config(&c);
        assert!(
            !fallback_indexed(&llm),
            "no endpoint created from empty URL"
        );
    }
}
