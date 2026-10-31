//! The owner's usage view: a small server-rendered page, behind the owner's login, showing the
//! budget, usage and estimated cost per caller, sphere and model, and the refusals.
//!
//! It is served on its own listener, apart from the API, so the reverse proxy can expose it to
//! the owner's browser while the API stays on the internal network.

pub mod chart;
pub mod login;
pub mod oidc;

use std::sync::Arc;

use askama::Template;
use askama_web::WebTemplate;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router, middleware};
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use serde::Deserialize;

use crate::Gateway;
use crate::money::Eur;
use crate::report::{MonthReport, Share, month_of};
use crate::sessions::Session;
use chart::Series;
pub use login::LoginSettings;

/// What the web UI needs.
pub struct WebState {
    pub gateway: Arc<Gateway>,
    /// `None` only for development on a loopback address: then everyone is the owner.
    pub login: Option<LoginSettings>,
    pub pending_logins: login::PendingLogins,
}

pub fn router(gateway: Arc<Gateway>, login: Option<LoginSettings>) -> Router {
    let state = Arc::new(WebState {
        gateway,
        login,
        pending_logins: login::PendingLogins::default(),
    });
    let owner_only = Router::new()
        .route("/", get(usage))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            login::require_owner,
        ));
    Router::new()
        .merge(owner_only)
        .route("/login", get(login::login))
        .route("/auth/callback", get(login::callback))
        .route("/logout", post(login::logout))
        .route("/logged-out", get(logged_out))
        .route("/assets/style.css", get(style))
        .route("/health", get(|| async { "ok" }))
        .with_state(state)
}

#[derive(Deserialize)]
struct UsageQuery {
    /// `YYYY-MM`; the current month without it.
    month: Option<String>,
}

#[derive(Template, WebTemplate)]
#[template(path = "usage.html")]
struct UsagePage {
    /// The logged-in owner; `None` without a login (development).
    owner: Option<String>,
    title: String,
    previous: Option<String>,
    next: Option<String>,
    current: bool,
    budget: String,
    spent: String,
    left: String,
    used_percent: u64,
    /// The meter's width: the used share, at most 100.
    meter_percent: u64,
    over_budget: bool,
    calls: String,
    inputs: String,
    tokens: String,
    refusal_total: String,
    chart_html: Option<String>,
    by_caller: Vec<ShareRow>,
    by_sphere: Vec<ShareRow>,
    by_model: Vec<ShareRow>,
    refusals: Vec<RefusalRow>,
    history: Vec<HistoryRow>,
}

struct ShareRow {
    key: String,
    calls: String,
    inputs: String,
    tokens: String,
    cost: String,
}

struct RefusalRow {
    reason: &'static str,
    caller: String,
    sphere: String,
    count: String,
    last: String,
}

struct HistoryRow {
    month: String,
    link: String,
    calls: String,
    tokens: String,
    cost: String,
    refusals: String,
}

async fn usage(
    State(state): State<Arc<WebState>>,
    Extension(session): Extension<Session>,
    Query(query): Query<UsageQuery>,
) -> Response {
    let now = (state.gateway.clock)();
    let this_month = month_of(now);
    let month = match query.month.as_deref() {
        None | Some("") => this_month,
        Some(text) => match parse_month(text) {
            Some(month) if month <= this_month => month,
            Some(_) => {
                return message(
                    StatusCode::NOT_FOUND,
                    "Not found",
                    "That month has not begun yet.",
                );
            }
            None => {
                return message(
                    StatusCode::BAD_REQUEST,
                    "Invalid month",
                    "Months are written like 2026-10.",
                );
            }
        },
    };
    let report = match MonthReport::build(&state.gateway.store, state.gateway.budget, month, now) {
        Ok(report) => report,
        Err(err) => {
            tracing::error!(%err, "building the usage report failed");
            return message(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Error",
                "Something went wrong while loading the usage.",
            );
        }
    };
    let owner = state.login.is_some().then_some(session.owner_name);
    page(&report, owner).into_response()
}

fn page(report: &MonthReport, owner: Option<String>) -> UsagePage {
    let previous = report.month.checked_sub(1.month()).ok();
    UsagePage {
        owner,
        title: month_name(report.month),
        previous: previous.map(month_param),
        next: (!report.current)
            .then(|| report.month.checked_add(1.month()).ok().map(month_param))
            .flatten(),
        current: report.current,
        budget: money(report.budget),
        spent: money(report.totals.cost),
        left: money(report.left),
        used_percent: report.used_percent(),
        meter_percent: report.used_percent().min(100),
        over_budget: report.totals.cost >= report.budget,
        calls: count(report.totals.calls),
        inputs: count(report.totals.inputs),
        tokens: count(report.totals.input_tokens),
        refusal_total: count(report.refusal_total()),
        chart_html: daily_chart(report),
        by_caller: share_rows(&report.by_caller),
        by_sphere: share_rows(&report.by_sphere),
        by_model: share_rows(&report.by_model),
        refusals: report
            .refusals
            .iter()
            .map(|r| RefusalRow {
                reason: reason_label(&r.reason),
                caller: r.caller.clone(),
                sphere: r.sphere.clone(),
                count: count(r.count),
                last: time(r.last),
            })
            .collect(),
        history: report
            .history
            .iter()
            .map(|m| HistoryRow {
                month: month_name(m.month),
                link: month_param(m.month),
                calls: count(m.totals.calls),
                tokens: count(m.totals.input_tokens),
                cost: money(m.totals.cost),
                refusals: count(m.refusals),
            })
            .collect(),
    }
}

/// The cost per day and caller as a chart, if anything was used.
fn daily_chart(report: &MonthReport) -> Option<String> {
    let days = report.daily.first()?.days.len();
    let labels: Vec<String> = (0..days)
        .map(|day| {
            let date = report.month + (day as i64).days();
            date.strftime("%-d %b").to_string()
        })
        .collect();
    let series: Vec<Series> = report
        .daily
        .iter()
        .enumerate()
        .map(|(i, daily)| Series {
            label: daily.caller.clone(),
            color: format!("var(--line-{})", i % 6),
            values: daily.days.iter().map(|e| e.as_f64()).collect(),
        })
        .collect();
    Some(chart::line_chart(
        "Estimated cost per day and caller",
        &labels,
        &series,
        |eur| money(Eur::from_nano((eur * 1e9).round() as i64)),
    ))
}

fn share_rows(shares: &[Share]) -> Vec<ShareRow> {
    shares
        .iter()
        .map(|s| ShareRow {
            key: s.key.clone(),
            calls: count(s.totals.calls),
            inputs: count(s.totals.inputs),
            tokens: count(s.totals.input_tokens),
            cost: money(s.totals.cost),
        })
        .collect()
}

/// An amount for people: cents from one euro up, otherwise enough decimals to see small
/// embedding costs.
pub fn money(amount: Eur) -> String {
    let eur = amount.as_f64();
    if amount == Eur::ZERO {
        "0.00 €".into()
    } else if eur >= 1.0 {
        format!("{eur:.2} €")
    } else if eur >= 0.0001 {
        format!("{eur:.4} €")
    } else {
        "< 0.0001 €".into()
    }
}

/// A count for people, in groups of three digits separated by narrow no-break spaces.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut grouped = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push('\u{202f}');
        }
        grouped.push(digit);
    }
    grouped
}

fn reason_label(reason: &str) -> &'static str {
    match reason {
        "budget_exhausted" => "budget exhausted",
        "rate_limited" => "rate limited",
        "sphere_rule" => "sphere rule",
        _ => "other",
    }
}

fn parse_month(text: &str) -> Option<Date> {
    let (year, month) = text.split_once('-')?;
    if year.len() != 4 || month.len() != 2 {
        return None;
    }
    Date::new(year.parse().ok()?, month.parse().ok()?, 1).ok()
}

fn month_param(month: Date) -> String {
    month.strftime("%Y-%m").to_string()
}

fn month_name(month: Date) -> String {
    month.strftime("%B %Y").to_string()
}

fn time(at: Timestamp) -> String {
    at.to_zoned(TimeZone::UTC)
        .strftime("%Y-%m-%d %H:%M UTC")
        .to_string()
}

#[derive(Template, WebTemplate)]
#[template(path = "message.html")]
struct MessagePage<'a> {
    title: &'a str,
    text: &'a str,
}

/// A page with a message, e.g. why a login failed.
pub fn message(status: StatusCode, title: &str, text: &str) -> Response {
    (status, MessagePage { title, text }).into_response()
}

async fn logged_out() -> Response {
    message(
        StatusCode::OK,
        "Logged out",
        "You are logged out of the gateway's usage view.",
    )
}

async fn style() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        include_str!("../../assets/style.css"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_for_people() {
        let eur = |s: &str| s.parse::<Eur>().unwrap();
        assert_eq!(money(Eur::ZERO), "0.00 €");
        assert_eq!(money(eur("12.345")), "12.35 €");
        assert_eq!(money(eur("1")), "1.00 €");
        assert_eq!(money(eur("0.0123")), "0.0123 €");
        assert_eq!(money(eur("0.000002")), "< 0.0001 €");
    }

    #[test]
    fn counts_for_people() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1\u{202f}000");
        assert_eq!(count(2243174), "2\u{202f}243\u{202f}174");
    }

    #[test]
    fn months_in_links() {
        assert_eq!(parse_month("2026-10"), Some(Date::constant(2026, 10, 1)));
        assert_eq!(parse_month("2026-13"), None);
        assert_eq!(parse_month("2026-1"), None);
        assert_eq!(parse_month("october"), None);
        assert_eq!(month_param(Date::constant(2026, 9, 1)), "2026-09");
        assert_eq!(month_name(Date::constant(2026, 9, 1)), "September 2026");
    }
}
