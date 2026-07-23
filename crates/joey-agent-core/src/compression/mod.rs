//! Context compression subsystem (port of `agent/context_compressor.py`,
//! `agent/conversation_compression.py`, `agent/context_engine.py`,
//! `agent/context_breakdown.py`, `agent/manual_compression_feedback.py`,
//! and the offline core of `agent/model_metadata.py`).

pub mod anchors;
pub mod breakdown;
pub mod catalog;
pub mod compressor;
pub mod engine;
pub mod estimator;
pub mod feedback;
#[cfg(test)]
#[allow(clippy::await_holding_lock)]
mod loop_tests;
pub mod orchestrator;
pub mod summary;

/// Scripted summary backend shared by the compression test suites.
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
pub(crate) mod test_support {
    use super::summary::SummaryBackend;
    use async_trait::async_trait;
    use joey_providers::ProviderError;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    pub(crate) struct ScriptedSummary {
        responses: Mutex<VecDeque<Result<String, String>>>,
        pub(crate) prompts: Mutex<Vec<String>>,
        pub(crate) has_provider: bool,
        pub(crate) model: String,
    }

    impl ScriptedSummary {
        /// Endless successful summaries with the given body.
        pub(crate) fn ok(body: &str) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(VecDeque::from(vec![Ok(body.to_string()); 32])),
                prompts: Mutex::new(Vec::new()),
                has_provider: true,
                model: "aux-model".to_string(),
            })
        }

        /// A scripted sequence; errors are rebuilt as ProviderError variants
        /// by tag ("timeout", "connection", "auth", "format", "parse",
        /// "model_not_found").
        pub(crate) fn script(script: Vec<Result<String, &'static str>>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(
                    script.into_iter().map(|r| r.map_err(str::to_string)).collect(),
                ),
                prompts: Mutex::new(Vec::new()),
                has_provider: true,
                model: "aux-model".to_string(),
            })
        }

        pub(crate) fn no_provider() -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(VecDeque::new()),
                prompts: Mutex::new(Vec::new()),
                has_provider: false,
                model: String::new(),
            })
        }

        pub(crate) fn call_count(&self) -> usize {
            self.prompts.lock().unwrap().len()
        }

        fn error_for(tag: &str) -> ProviderError {
            match tag {
                "timeout" => ProviderError::Timeout("timed out".into()),
                "connection" => ProviderError::Connection("connection reset by peer".into()),
                "auth" => ProviderError::Auth("invalid api key".into()),
                "format" => ProviderError::FormatError("bad request".into()),
                "parse" => ProviderError::Parse("expecting value: line 1".into()),
                "model_not_found" => {
                    ProviderError::ModelNotFound("the model does not exist".into())
                }
                other => ProviderError::Other(other.to_string()),
            }
        }
    }

    #[async_trait]
    impl SummaryBackend for ScriptedSummary {
        async fn generate(
            &self,
            prompt: &str,
            _model_override: Option<&str>,
        ) -> Result<String, ProviderError> {
            self.prompts.lock().unwrap().push(prompt.to_string());
            match self.responses.lock().unwrap().pop_front() {
                Some(Ok(body)) => Ok(body),
                Some(Err(tag)) => Err(Self::error_for(&tag)),
                None => Ok("## Goal\nscripted default summary".to_string()),
            }
        }

        fn has_provider(&self) -> bool {
            self.has_provider
        }

        fn resolved_model(&self) -> String {
            self.model.clone()
        }

        fn resolved_provider(&self) -> String {
            "scripted".to_string()
        }

        fn aux_context_length(&self) -> i64 {
            256_000
        }
    }
}

pub use anchors::{ensure_compressed_has_user_turn, is_real_user_message};
pub use catalog::{
    get_context_length_from_provider_error, get_model_context_length,
    parse_available_output_tokens_from_error, parse_context_limit_from_error,
    DEFAULT_FALLBACK_CONTEXT, MINIMUM_CONTEXT_LENGTH,
};
pub use compressor::{
    ContextCompressor, LEGACY_SUMMARY_PREFIX, MERGED_SUMMARY_DELIMITER, SUMMARY_END_MARKER,
    SUMMARY_PREFIX,
};
pub use engine::{sanitize_memory_context, ContextEngine, UsageUpdate};
pub use estimator::{
    estimate_messages_tokens_rough, estimate_request_tokens_rough, estimate_tools_tokens_rough,
};
pub use feedback::{summarize_manual_compression, ManualCompressionSummary};
pub use orchestrator::{COMPACTION_STATUS, COMPACTION_STATUS_MARKER};
pub use summary::{AuxSummaryBackend, SummaryBackend};
