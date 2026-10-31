//! The LLM gateway: services call it instead of an LLM provider; it holds the provider key,
//! records usage per caller, sphere, provider and model, and enforces the spheres' rules, a
//! rate limit per caller and a monthly budget.

pub mod api;
pub mod callers;
pub mod limits;
pub mod money;
pub mod provider;
pub mod report;
pub mod retry;
pub mod sessions;
pub mod spheres;
pub mod usage;
pub mod web;

use std::sync::Arc;
use std::time::Duration;

use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};

pub use callers::Callers;
pub use limits::RateLimit;
pub use money::{Eur, Prices};
pub use provider::{Embeddings, Mistral, Provider, ProviderError};
pub use retry::RetryPolicy;
pub use spheres::{SphereError, Spheres};
pub use usage::{Refusal, StoreError, Usage, UsageStore};

/// The default monthly budget for all callers together.
pub const DEFAULT_BUDGET: &str = "10";
/// The default prices, in euros per million input tokens.
pub const DEFAULT_PRICES: &str = "mistral-embed=0.1";
/// The most texts one embeddings request may contain.
pub const MAX_INPUTS: usize = 512;

/// The current time; replaced in tests to move between months.
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(Timestamp::now)
}

/// Whether `s` is a valid caller or sphere name: lowercase letters, digits, `-` and `_`.
pub fn is_name(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The budget period containing `now`: the calendar month in UTC, as its start and the
/// start of the next month.
pub fn budget_period(now: Timestamp) -> (Timestamp, Timestamp) {
    let date = now.to_zoned(TimeZone::UTC).date().first_of_month();
    let start = date
        .to_zoned(TimeZone::UTC)
        .expect("the first of a month at midnight UTC exists");
    let next = start
        .checked_add(1.month())
        .expect("the next month is in range");
    (start.timestamp(), next.timestamp())
}

/// Everything the gateway needs to answer requests.
pub struct Gateway {
    pub callers: Callers,
    pub prices: Prices,
    pub budget: Eur,
    pub spheres: Spheres,
    pub rate_limit: RateLimit,
    pub retry: RetryPolicy,
    pub provider: Arc<dyn Provider>,
    pub store: UsageStore,
    pub clock: Clock,
}

/// An embeddings request from an authenticated caller.
#[derive(Debug, Clone)]
pub struct EmbedRequest {
    pub caller: String,
    pub sphere: String,
    pub model: String,
    pub inputs: Vec<String>,
}

/// Embeddings with what they used and cost.
#[derive(Debug, Clone)]
pub struct Embedded {
    pub embeddings: Embeddings,
    pub cost: Eur,
}

/// Why a request was not answered.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("{0}")]
    InvalidRequest(String),
    #[error("the model {0:?} is not offered by this gateway")]
    UnknownModel(String),
    #[error("the sphere {0:?} is not declared in the gateway's configuration")]
    UnknownSphere(String),
    #[error("the rules of sphere {sphere:?} do not allow sending its content to {provider}")]
    SphereRule { sphere: String, provider: String },
    #[error("the caller's rate limit of {per_minute} texts per minute is reached")]
    RateLimited {
        per_minute: u32,
        retry_after: Duration,
    },
    #[error("the monthly budget of {budget} is used up until {until}")]
    BudgetExhausted {
        budget: Eur,
        until: Timestamp,
        /// Seconds from now until `until`.
        retry_after: i64,
    },
    #[error("the provider failed: {0}")]
    Provider(#[from] ProviderError),
    #[error("the usage database failed: {0}")]
    Store(#[from] StoreError),
}

impl Gateway {
    /// Answers an embeddings request, or refuses it before anything reaches the provider.
    ///
    /// The checks run in this order: the request itself, the sphere's rules, the budget, the
    /// caller's rate limit. Refusals for the last three are recorded. The budget is checked
    /// before the call, so concurrent calls may overrun it by what they cost; the next call is
    /// then refused. Transient provider failures are retried within the retry policy's time
    /// limit.
    pub async fn embed(&self, request: EmbedRequest) -> Result<Embedded, EmbedError> {
        if !is_name(&request.sphere) {
            return Err(EmbedError::InvalidRequest(format!(
                "{:?} is not a sphere name (lowercase letters, digits, - and _)",
                request.sphere
            )));
        }
        if request.inputs.is_empty() {
            return Err(EmbedError::InvalidRequest("inputs is empty".into()));
        }
        if request.inputs.len() > MAX_INPUTS {
            return Err(EmbedError::InvalidRequest(format!(
                "inputs has {} texts; at most {MAX_INPUTS} are allowed",
                request.inputs.len()
            )));
        }
        let per_minute = self.rate_limit.per_minute();
        if request.inputs.len() > per_minute as usize {
            return Err(EmbedError::InvalidRequest(format!(
                "inputs has {} texts, more than the rate limit of {per_minute} per minute allows",
                request.inputs.len()
            )));
        }
        if !self.prices.offers(&request.model) {
            return Err(EmbedError::UnknownModel(request.model));
        }

        let now = (self.clock)();
        let provider = self.provider.name();
        match self.spheres.check(&request.sphere, provider) {
            Ok(()) => {}
            Err(SphereError::Unknown(sphere)) => return Err(EmbedError::UnknownSphere(sphere)),
            Err(SphereError::ProviderNotAllowed { sphere, provider }) => {
                self.refuse(now, &request, "sphere_rule")?;
                return Err(EmbedError::SphereRule { sphere, provider });
            }
        }

        let (month_start, next_month) = budget_period(now);
        if self.store.spent_since(month_start)? >= self.budget {
            self.refuse(now, &request, "budget_exhausted")?;
            return Err(EmbedError::BudgetExhausted {
                budget: self.budget,
                until: next_month,
                retry_after: (next_month.as_second() - now.as_second()).max(1),
            });
        }

        if let Err(retry_after) = self.rate_limit.take(&request.caller, request.inputs.len()) {
            self.refuse(now, &request, "rate_limited")?;
            return Err(EmbedError::RateLimited {
                per_minute,
                retry_after,
            });
        }

        let embeddings = self
            .retry
            .run(|| self.provider.embed(&request.model, &request.inputs))
            .await?;
        let cost = self
            .prices
            .cost(&request.model, embeddings.input_tokens)
            .expect("the model has a price; checked above");
        self.store.record_usage(&Usage {
            at: (self.clock)(),
            caller: request.caller,
            sphere: request.sphere,
            provider: provider.to_string(),
            model: request.model,
            inputs: request.inputs.len() as u64,
            input_tokens: embeddings.input_tokens,
            cost,
        })?;
        Ok(Embedded { embeddings, cost })
    }

    fn refuse(
        &self,
        at: Timestamp,
        request: &EmbedRequest,
        reason: &str,
    ) -> Result<(), StoreError> {
        self.store.record_refusal(&Refusal {
            at,
            caller: request.caller.clone(),
            sphere: request.sphere.clone(),
            model: request.model.clone(),
            reason: reason.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_period_is_the_utc_calendar_month() {
        let at = |s: &str| s.parse::<Timestamp>().unwrap();
        assert_eq!(
            budget_period(at("2026-10-31T23:59:59.999Z")),
            (at("2026-10-01T00:00:00Z"), at("2026-11-01T00:00:00Z"))
        );
        assert_eq!(
            budget_period(at("2026-11-01T00:00:00Z")),
            (at("2026-11-01T00:00:00Z"), at("2026-12-01T00:00:00Z"))
        );
        assert_eq!(
            budget_period(at("2026-12-15T08:00:00Z")),
            (at("2026-12-01T00:00:00Z"), at("2027-01-01T00:00:00Z"))
        );
    }

    #[test]
    fn names() {
        assert!(is_name("work"));
        assert!(is_name("club-2_b"));
        assert!(!is_name(""));
        assert!(!is_name("Work"));
        assert!(!is_name("wörk"));
        assert!(!is_name(&"a".repeat(65)));
    }
}
