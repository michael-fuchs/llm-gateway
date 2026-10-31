//! Retrying provider calls that failed transiently, within a time limit.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

use crate::provider::ProviderError;

/// How the gateway retries a provider call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The longest a call may take in total, retries and waits included.
    pub time_limit: Duration,
    /// The longest a single attempt may wait for the provider's answer.
    pub attempt_timeout: Duration,
    /// The wait after the first failure; it doubles after each further failure.
    pub first_backoff: Duration,
    /// The longest wait between two attempts.
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            time_limit: Duration::from_secs(60),
            attempt_timeout: Duration::from_secs(30),
            first_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(10),
        }
    }
}

impl RetryPolicy {
    /// Runs `attempt` until it succeeds, fails permanently, or the time limit would be
    /// exceeded. Waits between attempts back off exponentially, or as long as the provider
    /// asked; a wait that would end after the time limit is not started.
    pub async fn run<F, Fut, T>(&self, mut attempt: F) -> Result<T, ProviderError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let start = Instant::now();
        let deadline = start + self.time_limit;
        let mut backoff = self.first_backoff;
        let mut attempts = 0u32;
        loop {
            attempts += 1;
            let timeout = self
                .attempt_timeout
                .min(deadline.saturating_duration_since(Instant::now()));
            let err = match tokio::time::timeout(timeout, attempt()).await {
                Ok(Ok(value)) => return Ok(value),
                Ok(Err(err)) if !err.transient => return Err(err),
                Ok(Err(err)) => err,
                Err(_) => {
                    ProviderError::transient(format!("no answer within {}s", timeout.as_secs_f64()))
                }
            };

            let wait = err.retry_after.unwrap_or(backoff);
            if Instant::now() + wait >= deadline {
                return Err(ProviderError {
                    message: format!(
                        "gave up after {attempts} attempt{} in {:.1}s: {}",
                        if attempts == 1 { "" } else { "s" },
                        start.elapsed().as_secs_f64(),
                        err.message
                    ),
                    ..err
                });
            }
            tracing::info!(attempt = attempts, error = %err, wait_ms = wait.as_millis() as u64, "provider call failed; retrying");
            tokio::time::sleep(wait).await;
            backoff = (backoff * 2).min(self.max_backoff);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const POLICY: RetryPolicy = RetryPolicy {
        time_limit: Duration::from_secs(10),
        attempt_timeout: Duration::from_secs(3),
        first_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(2),
    };

    /// Runs `POLICY` against scripted outcomes; returns the result, the attempts made and the
    /// time each attempt started.
    async fn run(
        script: Vec<Result<u32, ProviderError>>,
    ) -> (Result<u32, ProviderError>, Vec<Duration>) {
        let start = Instant::now();
        let script = Mutex::new(script.into_iter());
        let starts = Mutex::new(Vec::new());
        let result = POLICY
            .run(|| {
                starts.lock().unwrap().push(start.elapsed());
                let next = script.lock().unwrap().next();
                async move {
                    match next {
                        Some(outcome) => outcome,
                        // Out of script: the provider never answers.
                        None => std::future::pending().await,
                    }
                }
            })
            .await;
        (result, starts.into_inner().unwrap())
    }

    fn secs(s: &[f64]) -> Vec<Duration> {
        s.iter().map(|&s| Duration::from_secs_f64(s)).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn retries_transient_failures_with_backoff() {
        let (result, starts) = run(vec![
            Err(ProviderError::transient("503")),
            Err(ProviderError::transient("503")),
            Err(ProviderError::transient("503")),
            Ok(7),
        ])
        .await;
        assert_eq!(result, Ok(7));
        // Waits of 1s, 2s, then 2s (the most).
        assert_eq!(starts, secs(&[0.0, 1.0, 3.0, 5.0]));
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_retry_permanent_failures() {
        let (result, starts) = run(vec![
            Err(ProviderError::transient("503")),
            Err(ProviderError::permanent("401")),
            Ok(7),
        ])
        .await;
        assert_eq!(result, Err(ProviderError::permanent("401")));
        assert_eq!(starts.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn waits_as_long_as_the_provider_asks() {
        let (result, starts) = run(vec![
            Err(ProviderError::transient("429").with_retry_after(Some(Duration::from_secs(4)))),
            Ok(7),
        ])
        .await;
        assert_eq!(result, Ok(7));
        assert_eq!(starts, secs(&[0.0, 4.0]));
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_within_the_time_limit() {
        let failures = (0..20)
            .map(|_| Err(ProviderError::transient("503")))
            .collect();
        let (result, starts) = run(failures).await;
        let err = result.unwrap_err();
        assert!(err.transient);
        assert!(
            err.message
                .starts_with("gave up after 6 attempts in 9.0s: 503"),
            "{err}"
        );
        // 0, 1, 3, 5, 7, 9; the next wait would end at the limit.
        assert_eq!(starts, secs(&[0.0, 1.0, 3.0, 5.0, 7.0, 9.0]));
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_wait_past_the_time_limit_when_asked_to() {
        let (result, starts) = run(vec![
            Err(ProviderError::transient("429").with_retry_after(Some(Duration::from_secs(60)))),
            Ok(7),
        ])
        .await;
        assert!(
            result
                .unwrap_err()
                .message
                .starts_with("gave up after 1 attempt in 0.0s")
        );
        assert_eq!(starts.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_attempts_and_stops_at_the_time_limit() {
        let start = Instant::now();
        // The provider never answers: attempts of 3s, 3s and 1s (what is left) with waits of
        // 1s and 2s in between make up exactly the 10s limit.
        let (result, starts) = run(vec![]).await;
        let err = result.unwrap_err();
        assert!(err.message.contains("no answer within"), "{err}");
        assert_eq!(starts, secs(&[0.0, 4.0, 9.0]));
        assert!(
            start.elapsed() <= POLICY.time_limit,
            "{:?}",
            start.elapsed()
        );
    }
}
