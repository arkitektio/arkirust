//! Backoff between reconnect attempts.

use core::time::Duration;

/// Exponential backoff between reconnect attempts.
#[derive(Debug, Clone)]
pub struct ConnectionPolicy {
    pub max_retries: usize,
    pub initial: Duration,
    pub factor: f64,
    pub max: Duration,
    pub jitter: f64,
    /// A connection that stayed up this long resets the retry budget.
    pub reset_after: Duration,
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial: Duration::from_secs(1),
            factor: 2.0,
            max: Duration::from_secs(60),
            jitter: 0.1,
            reset_after: Duration::from_secs(30),
        }
    }
}

impl ConnectionPolicy {
    /// The wait before `attempt` (1, 2, …), with `sample` in `[-1, 1]` scaling the jitter.
    pub fn delay_for(&self, attempt: usize, sample: f64) -> Duration {
        let max = self.max.as_secs_f64();
        let mut base = self.initial.as_secs_f64();
        for _ in 1..attempt {
            if base >= max {
                break;
            }
            base *= self.factor;
        }
        let base = base.min(max);
        let delay = base + base * self.jitter * sample;
        Duration::from_secs_f64(if delay > 0.0 { delay } else { 0.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_up_to_the_cap() {
        let policy = ConnectionPolicy {
            jitter: 0.0,
            max: Duration::from_secs(5),
            ..Default::default()
        };
        let delays: std::vec::Vec<u64> = (1..=5)
            .map(|a| policy.delay_for(a, 1.0).as_secs())
            .collect();
        assert_eq!(delays, [1, 2, 4, 5, 5]);
        let jittered = ConnectionPolicy::default().delay_for(2, -1.0);
        assert_eq!(jittered, Duration::from_secs_f64(1.8));
    }
}
