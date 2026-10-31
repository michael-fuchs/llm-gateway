//! The numbers of the usage view: the budget, usage and estimated cost per caller, sphere and
//! model over a month, and the refusals. Computed from the usage database, separate from
//! their HTML so they can be tested.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use rusqlite::params;

use crate::money::Eur;
use crate::usage::{StoreError, UsageStore, timestamp};

const DAY_MS: i64 = 24 * 3600 * 1000;

/// What answered calls added up to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub calls: u64,
    pub inputs: u64,
    pub input_tokens: u64,
    pub cost: Eur,
}

/// What usage is broken down by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Caller,
    Sphere,
    Model,
}

impl Dimension {
    fn column(self) -> &'static str {
        match self {
            Dimension::Caller => "caller",
            Dimension::Sphere => "sphere",
            Dimension::Model => "model",
        }
    }
}

/// The usage of one caller, sphere or model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub key: String,
    pub totals: Totals,
}

/// Refusals of one reason, caller and sphere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusalCount {
    pub reason: String,
    pub caller: String,
    pub sphere: String,
    pub count: u64,
    pub last: Timestamp,
}

/// The estimated cost per day of one caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyCost {
    pub caller: String,
    /// One amount per day of the month, up to the last day shown.
    pub days: Vec<Eur>,
}

/// One month in the overview of past months.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthSummary {
    /// The first day of the month.
    pub month: Date,
    pub totals: Totals,
    pub refusals: u64,
}

/// Everything the usage view shows for one month.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthReport {
    /// The first day of the month (UTC).
    pub month: Date,
    /// Whether this is the current month, whose budget is still being used.
    pub current: bool,
    pub budget: Eur,
    pub totals: Totals,
    /// What is left of the budget; zero once it is used up.
    pub left: Eur,
    pub by_caller: Vec<Share>,
    pub by_sphere: Vec<Share>,
    pub by_model: Vec<Share>,
    /// Cost per day and caller, from the first day to today (or the month's last day).
    pub daily: Vec<DailyCost>,
    pub refusals: Vec<RefusalCount>,
    /// The months before this one with any usage or refusals, newest first, at most 12.
    pub history: Vec<MonthSummary>,
}

/// The first day of the month containing `at`, in UTC (the budget's month).
pub fn month_of(at: Timestamp) -> Date {
    at.to_zoned(TimeZone::UTC).date().first_of_month()
}

fn start_of(month: Date) -> Timestamp {
    month
        .to_zoned(TimeZone::UTC)
        .expect("midnight UTC exists")
        .timestamp()
}

impl MonthReport {
    /// The report of `month` (its first day), as seen at `now`.
    pub fn build(
        store: &UsageStore,
        budget: Eur,
        month: Date,
        now: Timestamp,
    ) -> Result<Self, StoreError> {
        let month = month.first_of_month();
        let next = month.checked_add(1.month()).expect("in range");
        let (from, to) = (start_of(month), start_of(next));
        let current = month == month_of(now);

        let totals = store.totals(from, to)?;
        let shown_days = if current {
            (now.as_millisecond() - from.as_millisecond()) / DAY_MS + 1
        } else {
            i64::from(month.days_in_month())
        };
        let mut history = Vec::new();
        let mut earlier = month;
        for _ in 0..12 {
            earlier = earlier.checked_sub(1.month()).expect("in range");
            let (from, to) = (start_of(earlier), start_of(month_after(earlier)));
            let summary = MonthSummary {
                month: earlier,
                totals: store.totals(from, to)?,
                refusals: store.refusal_count(from, to)?,
            };
            if summary.totals.calls > 0 || summary.refusals > 0 {
                history.push(summary);
            }
        }

        Ok(MonthReport {
            month,
            current,
            budget,
            totals,
            left: Eur::from_nano((budget.nano() - totals.cost.nano()).max(0)),
            by_caller: store.shares(Dimension::Caller, from, to)?,
            by_sphere: store.shares(Dimension::Sphere, from, to)?,
            by_model: store.shares(Dimension::Model, from, to)?,
            daily: store.daily_cost(from, to, shown_days.max(1) as usize)?,
            refusals: store.refusal_counts(from, to)?,
            history,
        })
    }

    /// The share of the budget used, in percent (more than 100 when it was overrun).
    pub fn used_percent(&self) -> u64 {
        if self.budget.nano() <= 0 {
            return 100;
        }
        (self.totals.cost.nano() as i128 * 100 / self.budget.nano() as i128) as u64
    }

    pub fn refusal_total(&self) -> u64 {
        self.refusals.iter().map(|r| r.count).sum()
    }
}

fn month_after(month: Date) -> Date {
    month.checked_add(1.month()).expect("in range")
}

impl UsageStore {
    /// What all calls in `[from, to)` added up to.
    pub fn totals(&self, from: Timestamp, to: Timestamp) -> Result<Totals, StoreError> {
        Ok(self.conn().query_row(
            "SELECT count(*), coalesce(sum(inputs), 0), coalesce(sum(input_tokens), 0), \
                    coalesce(sum(cost_nano_eur), 0) \
             FROM usage WHERE at >= ?1 AND at < ?2",
            params![from.as_millisecond(), to.as_millisecond()],
            totals_from_row,
        )?)
    }

    /// Usage in `[from, to)` per caller, sphere or model, most expensive first.
    pub fn shares(
        &self,
        by: Dimension,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<Share>, StoreError> {
        let conn = self.conn();
        // The column comes from a fixed list, never from input.
        let column = by.column();
        let mut stmt = conn.prepare(&format!(
            "SELECT count(*), sum(inputs), sum(input_tokens), sum(cost_nano_eur), {column} \
             FROM usage WHERE at >= ?1 AND at < ?2 \
             GROUP BY {column} ORDER BY sum(cost_nano_eur) DESC, {column}"
        ))?;
        let rows = stmt.query_map(params![from.as_millisecond(), to.as_millisecond()], |row| {
            Ok(Share {
                key: row.get(4)?,
                totals: totals_from_row(row)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The cost per day and caller for the first `days` days from `from` (and before `to`).
    pub fn daily_cost(
        &self,
        from: Timestamp,
        to: Timestamp,
        days: usize,
    ) -> Result<Vec<DailyCost>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT caller, (at - ?1) / ?3 AS day, sum(cost_nano_eur) \
             FROM usage WHERE at >= ?1 AND at < ?2 GROUP BY caller, day ORDER BY caller",
        )?;
        let rows = stmt.query_map(
            params![from.as_millisecond(), to.as_millisecond(), DAY_MS],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        let mut daily: Vec<DailyCost> = Vec::new();
        for row in rows {
            let (caller, day, nano) = row?;
            if daily.last().is_none_or(|d| d.caller != caller) {
                daily.push(DailyCost {
                    caller,
                    days: vec![Eur::ZERO; days],
                });
            }
            if let Some(slot) = daily.last_mut().unwrap().days.get_mut(day as usize) {
                *slot = Eur::from_nano(nano);
            }
        }
        Ok(daily)
    }

    /// Refusals in `[from, to)` per reason, caller and sphere, most frequent first.
    pub fn refusal_counts(
        &self,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<RefusalCount>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT reason, caller, sphere, count(*), max(at) FROM refusal \
             WHERE at >= ?1 AND at < ?2 GROUP BY reason, caller, sphere \
             ORDER BY count(*) DESC, max(at) DESC",
        )?;
        let rows = stmt.query_map(params![from.as_millisecond(), to.as_millisecond()], |row| {
            Ok(RefusalCount {
                reason: row.get(0)?,
                caller: row.get(1)?,
                sphere: row.get(2)?,
                count: row.get::<_, i64>(3)? as u64,
                last: timestamp(row.get(4)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn refusal_count(&self, from: Timestamp, to: Timestamp) -> Result<u64, StoreError> {
        let count: i64 = self.conn().query_row(
            "SELECT count(*) FROM refusal WHERE at >= ?1 AND at < ?2",
            params![from.as_millisecond(), to.as_millisecond()],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }
}

/// Reads count, inputs, tokens and cost from the first four columns.
fn totals_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Totals> {
    Ok(Totals {
        calls: row.get::<_, i64>(0)? as u64,
        inputs: row.get::<_, i64>(1)? as u64,
        input_tokens: row.get::<_, i64>(2)? as u64,
        cost: Eur::from_nano(row.get(3)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{Refusal, Usage};

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn date(s: &str) -> Date {
        s.parse().unwrap()
    }

    fn eur(s: &str) -> Eur {
        s.parse().unwrap()
    }

    fn call(store: &UsageStore, when: &str, caller: &str, sphere: &str, model: &str, cost: &str) {
        store
            .record_usage(&Usage {
                at: at(when),
                caller: caller.into(),
                sphere: sphere.into(),
                provider: "mistral".into(),
                model: model.into(),
                inputs: 2,
                input_tokens: 100,
                cost: eur(cost),
            })
            .unwrap();
    }

    fn refuse(store: &UsageStore, when: &str, caller: &str, sphere: &str, reason: &str) {
        store
            .record_refusal(&Refusal {
                at: at(when),
                caller: caller.into(),
                sphere: sphere.into(),
                model: "mistral-embed".into(),
                reason: reason.into(),
            })
            .unwrap();
    }

    /// Usage over September and October 2026, seen on 3 October at noon.
    fn october() -> MonthReport {
        let store = UsageStore::in_memory().unwrap();
        // September: before the month shown, only in the history.
        call(
            &store,
            "2026-09-30T23:59:59.999Z",
            "mnemograph",
            "work",
            "mistral-embed",
            "1",
        );
        refuse(
            &store,
            "2026-09-15T10:00:00Z",
            "documents",
            "private",
            "rate_limited",
        );
        // October.
        call(
            &store,
            "2026-10-01T00:00:00Z",
            "mnemograph",
            "work",
            "mistral-embed",
            "0.5",
        );
        call(
            &store,
            "2026-10-01T08:00:00Z",
            "mnemograph",
            "private",
            "mistral-embed",
            "0.25",
        );
        call(
            &store,
            "2026-10-03T11:00:00Z",
            "documents",
            "work",
            "mistral-embed",
            "2",
        );
        call(
            &store,
            "2026-10-03T11:30:00Z",
            "documents",
            "work",
            "other-embed",
            "1.25",
        );
        refuse(
            &store,
            "2026-10-02T09:00:00Z",
            "documents",
            "secret",
            "sphere_rule",
        );
        refuse(
            &store,
            "2026-10-03T09:00:00Z",
            "documents",
            "secret",
            "sphere_rule",
        );
        refuse(
            &store,
            "2026-10-03T10:00:00Z",
            "mnemograph",
            "work",
            "budget_exhausted",
        );
        // November: after the month shown.
        call(
            &store,
            "2026-11-01T00:00:00Z",
            "mnemograph",
            "work",
            "mistral-embed",
            "7",
        );

        MonthReport::build(
            &store,
            eur("10"),
            date("2026-10-01"),
            at("2026-10-03T12:00:00Z"),
        )
        .unwrap()
    }

    #[test]
    fn budget_used_and_left_in_the_month() {
        let report = october();
        assert!(report.current);
        assert_eq!(
            report.totals,
            Totals {
                calls: 4,
                inputs: 8,
                input_tokens: 400,
                cost: eur("4"),
            }
        );
        assert_eq!(report.left, eur("6"));
        assert_eq!(report.used_percent(), 40);
    }

    #[test]
    fn usage_per_caller_sphere_and_model() {
        let report = october();
        let shares = |shares: &[Share]| -> Vec<(String, u64, Eur)> {
            shares
                .iter()
                .map(|s| (s.key.clone(), s.totals.calls, s.totals.cost))
                .collect()
        };
        assert_eq!(
            shares(&report.by_caller),
            vec![
                ("documents".into(), 2, eur("3.25")),
                ("mnemograph".into(), 2, eur("0.75")),
            ]
        );
        assert_eq!(
            shares(&report.by_sphere),
            vec![
                ("work".into(), 3, eur("3.75")),
                ("private".into(), 1, eur("0.25")),
            ]
        );
        assert_eq!(
            shares(&report.by_model),
            vec![
                ("mistral-embed".into(), 3, eur("2.75")),
                ("other-embed".into(), 1, eur("1.25")),
            ]
        );
    }

    #[test]
    fn cost_per_day_up_to_today() {
        let report = october();
        assert_eq!(
            report.daily,
            vec![
                DailyCost {
                    caller: "documents".into(),
                    days: vec![Eur::ZERO, Eur::ZERO, eur("3.25")],
                },
                DailyCost {
                    caller: "mnemograph".into(),
                    days: vec![eur("0.75"), Eur::ZERO, Eur::ZERO],
                },
            ]
        );
    }

    #[test]
    fn refusals_by_reason_caller_and_sphere() {
        let report = october();
        assert_eq!(report.refusal_total(), 3);
        assert_eq!(
            report.refusals,
            vec![
                RefusalCount {
                    reason: "sphere_rule".into(),
                    caller: "documents".into(),
                    sphere: "secret".into(),
                    count: 2,
                    last: at("2026-10-03T09:00:00Z"),
                },
                RefusalCount {
                    reason: "budget_exhausted".into(),
                    caller: "mnemograph".into(),
                    sphere: "work".into(),
                    count: 1,
                    last: at("2026-10-03T10:00:00Z"),
                },
            ]
        );
    }

    #[test]
    fn history_lists_earlier_months_with_activity() {
        let report = october();
        assert_eq!(
            report.history,
            vec![MonthSummary {
                month: date("2026-09-01"),
                totals: Totals {
                    calls: 1,
                    inputs: 2,
                    input_tokens: 100,
                    cost: eur("1"),
                },
                refusals: 1,
            }]
        );
    }

    #[test]
    fn a_past_month_shows_all_its_days_and_an_overrun_budget() {
        let store = UsageStore::in_memory().unwrap();
        call(
            &store,
            "2026-09-30T20:00:00Z",
            "mnemograph",
            "work",
            "mistral-embed",
            "12",
        );
        let report = MonthReport::build(
            &store,
            eur("10"),
            date("2026-09-14"),
            at("2026-10-03T12:00:00Z"),
        )
        .unwrap();
        assert_eq!(report.month, date("2026-09-01"));
        assert!(!report.current);
        assert_eq!(report.daily[0].days.len(), 30);
        assert_eq!(report.daily[0].days[29], eur("12"));
        assert_eq!(report.left, Eur::ZERO);
        assert_eq!(report.used_percent(), 120);
    }

    #[test]
    fn an_empty_month() {
        let store = UsageStore::in_memory().unwrap();
        let report = MonthReport::build(
            &store,
            eur("10"),
            date("2026-10-01"),
            at("2026-10-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(report.totals, Totals::default());
        assert_eq!(report.left, eur("10"));
        assert!(report.daily.is_empty() && report.refusals.is_empty() && report.history.is_empty());
    }
}
