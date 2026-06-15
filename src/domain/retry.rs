#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_worker_attempts: i64,
    pub max_outbox_attempts: i64,
    pub base_retry_delay_ms: i64,
    pub max_retry_delay_ms: i64,
    pub lease_timeout_ms: i64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_worker_attempts: 3,
            max_outbox_attempts: 3,
            base_retry_delay_ms: 1_000,
            max_retry_delay_ms: 30_000,
            lease_timeout_ms: 30_000,
        }
    }
}

pub fn next_retry_delay_ms(attempts: i64, base_ms: i64, max_ms: i64) -> i64 {
    let exponent = if attempts > 0 { attempts - 1 } else { 0 };
    let shift = 1u64.checked_shl(exponent as u32).unwrap_or(u64::MAX);
    let delay = (base_ms as u64).saturating_mul(shift.min(100_000));
    delay.min(max_ms as u64) as i64
}

pub fn compute_available_at(now_ms: i64, attempts: i64, base_ms: i64, max_ms: i64) -> i64 {
    now_ms + next_retry_delay_ms(attempts, base_ms, max_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_retry_delay_increases_exponentially() {
        let policy = RetryPolicy::default();
        let d0 = next_retry_delay_ms(1, policy.base_retry_delay_ms, policy.max_retry_delay_ms);
        let d1 = next_retry_delay_ms(2, policy.base_retry_delay_ms, policy.max_retry_delay_ms);
        assert!(d0 <= d1, "retry delay must not decrease");
        assert_eq!(d0, 1_000);
        assert_eq!(d1, 2_000);
    }

    #[test]
    fn next_retry_delay_caps_at_max() {
        let delay = next_retry_delay_ms(100, 1_000, 30_000);
        assert_eq!(delay, 30_000);
    }

    #[test]
    fn retry_delay_for_attempt_0_uses_base() {
        let delay = next_retry_delay_ms(0, 1_000, 30_000);
        assert_eq!(delay, 1_000);
    }
}
