use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerJobStatus {
    Queued,
    Leased,
    Running,
    Succeeded,
    RetryableFailed,
    Dead,
    Failed,
}

impl WorkerJobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkerJobStatus::Queued => "queued",
            WorkerJobStatus::Leased => "leased",
            WorkerJobStatus::Running => "running",
            WorkerJobStatus::Succeeded => "succeeded",
            WorkerJobStatus::RetryableFailed => "retryable_failed",
            WorkerJobStatus::Dead => "dead",
            WorkerJobStatus::Failed => "failed",
        }
    }

    pub fn parse_opt(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(WorkerJobStatus::Queued),
            "leased" => Some(WorkerJobStatus::Leased),
            "running" => Some(WorkerJobStatus::Running),
            "succeeded" => Some(WorkerJobStatus::Succeeded),
            "retryable_failed" => Some(WorkerJobStatus::RetryableFailed),
            "dead" => Some(WorkerJobStatus::Dead),
            "failed" => Some(WorkerJobStatus::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxDispatchStatus {
    Pending,
    Leased,
    Dispatching,
    Succeeded,
    Failed,
    RetryableFailed,
    Unknown,
    Dead,
}

impl OutboxDispatchStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            OutboxDispatchStatus::Pending => "pending",
            OutboxDispatchStatus::Leased => "leased",
            OutboxDispatchStatus::Dispatching => "dispatching",
            OutboxDispatchStatus::Succeeded => "succeeded",
            OutboxDispatchStatus::Failed => "failed",
            OutboxDispatchStatus::RetryableFailed => "retryable_failed",
            OutboxDispatchStatus::Unknown => "unknown",
            OutboxDispatchStatus::Dead => "dead",
        }
    }

    pub fn parse_opt(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(OutboxDispatchStatus::Pending),
            "leased" => Some(OutboxDispatchStatus::Leased),
            "dispatching" => Some(OutboxDispatchStatus::Dispatching),
            "succeeded" => Some(OutboxDispatchStatus::Succeeded),
            "failed" => Some(OutboxDispatchStatus::Failed),
            "retryable_failed" => Some(OutboxDispatchStatus::RetryableFailed),
            "unknown" => Some(OutboxDispatchStatus::Unknown),
            "dead" => Some(OutboxDispatchStatus::Dead),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_job_status_serde_roundtrip() -> Result<(), serde_json::Error> {
        let cases = [
            (WorkerJobStatus::Queued, "queued"),
            (WorkerJobStatus::Leased, "leased"),
            (WorkerJobStatus::Running, "running"),
            (WorkerJobStatus::Succeeded, "succeeded"),
            (WorkerJobStatus::RetryableFailed, "retryable_failed"),
            (WorkerJobStatus::Dead, "dead"),
            (WorkerJobStatus::Failed, "failed"),
        ];
        for (status, expected_str) in &cases {
            let serialized = serde_json::to_string(status)?;
            assert_eq!(serialized, format!("\"{}\"", expected_str));
            let deserialized: WorkerJobStatus = serde_json::from_str(&serialized)?;
            assert_eq!(deserialized, *status);
        }
        Ok(())
    }

    #[test]
    fn worker_job_status_as_str_matches_all_variants() {
        for status in &[
            WorkerJobStatus::Queued,
            WorkerJobStatus::Leased,
            WorkerJobStatus::Running,
            WorkerJobStatus::Succeeded,
            WorkerJobStatus::RetryableFailed,
            WorkerJobStatus::Dead,
            WorkerJobStatus::Failed,
        ] {
            let s = status.as_str();
            let parsed = WorkerJobStatus::parse_opt(s);
            assert_eq!(parsed, Some(*status));
        }
    }

    #[test]
    fn outbox_dispatch_status_serde_roundtrip() -> Result<(), serde_json::Error> {
        let cases = [
            (OutboxDispatchStatus::Pending, "pending"),
            (OutboxDispatchStatus::Leased, "leased"),
            (OutboxDispatchStatus::Dispatching, "dispatching"),
            (OutboxDispatchStatus::Succeeded, "succeeded"),
            (OutboxDispatchStatus::Failed, "failed"),
            (OutboxDispatchStatus::RetryableFailed, "retryable_failed"),
            (OutboxDispatchStatus::Unknown, "unknown"),
            (OutboxDispatchStatus::Dead, "dead"),
        ];
        for (status, expected_str) in &cases {
            let serialized = serde_json::to_string(status)?;
            assert_eq!(serialized, format!("\"{}\"", expected_str));
            let deserialized: OutboxDispatchStatus = serde_json::from_str(&serialized)?;
            assert_eq!(deserialized, *status);
        }
        Ok(())
    }

    #[test]
    fn outbox_dispatch_status_as_str_matches_all_variants() {
        for status in &[
            OutboxDispatchStatus::Pending,
            OutboxDispatchStatus::Leased,
            OutboxDispatchStatus::Dispatching,
            OutboxDispatchStatus::Succeeded,
            OutboxDispatchStatus::Failed,
            OutboxDispatchStatus::RetryableFailed,
            OutboxDispatchStatus::Unknown,
            OutboxDispatchStatus::Dead,
        ] {
            let s = status.as_str();
            let parsed = OutboxDispatchStatus::parse_opt(s);
            assert_eq!(parsed, Some(*status));
        }
    }
}
