//! The rate limit per caller, which protects the budget from a runaway caller.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// The default number of texts each caller may send for embedding per minute.
pub const DEFAULT_INPUTS_PER_MINUTE: u32 = 1000;

/// A token bucket per caller, counted in texts (inputs) since they, not requests, drive the
/// cost. A caller may use up a minute's allowance at once and then refills steadily.
pub struct RateLimit {
    per_minute: u32,
    buckets: Mutex<HashMap<String, Bucket>>,
}

struct Bucket {
    available: f64,
    at: Instant,
}

impl RateLimit {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            buckets: Mutex::default(),
        }
    }

    /// The most texts one request may contain to ever pass the limit.
    pub fn per_minute(&self) -> u32 {
        self.per_minute
    }

    /// Takes `inputs` from the caller's allowance, or says how long until it would suffice.
    pub fn take(&self, caller: &str, inputs: usize) -> Result<(), Duration> {
        let capacity = f64::from(self.per_minute);
        let per_second = capacity / 60.0;
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        let bucket = buckets.entry(caller.to_string()).or_insert(Bucket {
            available: capacity,
            at: now,
        });
        let elapsed = now.duration_since(bucket.at).as_secs_f64();
        bucket.available = (bucket.available + elapsed * per_second).min(capacity);
        bucket.at = now;

        let needed = inputs as f64;
        if needed <= bucket.available {
            bucket.available -= needed;
            Ok(())
        } else {
            Err(Duration::from_secs_f64(
                (needed - bucket.available) / per_second,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn allows_a_minute_at_once_then_refills() {
        let limit = RateLimit::new(60);
        assert_eq!(limit.take("a", 50), Ok(()));
        assert_eq!(limit.take("a", 10), Ok(()));
        // Empty: one text refills per second.
        assert_eq!(limit.take("a", 3), Err(Duration::from_secs(3)));
        // Each caller has its own allowance.
        assert_eq!(limit.take("b", 60), Ok(()));

        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(limit.take("a", 3), Err(Duration::from_secs(1)));
        assert_eq!(limit.take("a", 2), Ok(()));

        // Never more than a minute's allowance, however long the caller waits.
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert_eq!(limit.take("a", 60), Ok(()));
        assert!(limit.take("a", 1).is_err());
    }
}
