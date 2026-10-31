//! Amounts of money and prices per model.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// Nano-euros per euro: the smallest unit amounts are kept in, so that a single token of a
/// cheap model still costs a whole number.
const NANO_PER_EUR: i64 = 1_000_000_000;

/// An amount in euros, kept exactly as a whole number of nano-euros.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Eur(i64);

impl Eur {
    pub const ZERO: Eur = Eur(0);

    pub fn from_nano(nano: i64) -> Self {
        Eur(nano)
    }

    pub fn nano(self) -> i64 {
        self.0
    }

    /// The amount as a floating-point number of euros, for display and JSON.
    pub fn as_f64(self) -> f64 {
        self.0 as f64 / NANO_PER_EUR as f64
    }
}

impl FromStr for Eur {
    type Err = String;

    /// Parses a non-negative decimal number of euros such as `10`, `0.1` or `2.50`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("{s:?} is not an amount in euros like 10 or 0.25");
        let (whole, fraction) = s.trim().split_once('.').unwrap_or((s.trim(), ""));
        if whole.is_empty() && fraction.is_empty()
            || !whole
                .chars()
                .chain(fraction.chars())
                .all(|c| c.is_ascii_digit())
        {
            return Err(invalid());
        }
        if fraction.len() > 9 {
            return Err(format!("{s:?} has more than nine decimal places"));
        }
        let whole: i64 = if whole.is_empty() {
            0
        } else {
            whole.parse().map_err(|_| invalid())?
        };
        let fraction: i64 = if fraction.is_empty() {
            0
        } else {
            format!("{fraction:0<9}").parse().map_err(|_| invalid())?
        };
        whole
            .checked_mul(NANO_PER_EUR)
            .and_then(|nano| nano.checked_add(fraction))
            .map(Eur)
            .ok_or_else(|| format!("{s:?} is too large"))
    }
}

impl fmt::Display for Eur {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fraction = format!("{:09}", self.0 % NANO_PER_EUR);
        let fraction = fraction.trim_end_matches('0');
        write!(f, "{}.{:0<2} EUR", self.0 / NANO_PER_EUR, fraction)
    }
}

/// The price of each model the gateway offers, in euros per million input tokens.
///
/// A model without a price is not offered: requests for it are refused, so the budget
/// always knows what a call costs.
#[derive(Debug, Clone, Default)]
pub struct Prices(HashMap<String, Eur>);

impl Prices {
    /// Parses prices like `mistral-embed=0.1` (euros per million input tokens).
    pub fn parse<S: AsRef<str>>(entries: &[S]) -> Result<Self, String> {
        let mut prices = HashMap::new();
        for entry in entries {
            let entry = entry.as_ref();
            let (model, price) = entry
                .split_once('=')
                .ok_or_else(|| format!("{entry:?} is not a price like mistral-embed=0.1"))?;
            let model = model.trim();
            if model.is_empty() {
                return Err(format!("{entry:?} names no model"));
            }
            if prices.insert(model.to_string(), price.parse()?).is_some() {
                return Err(format!("the model {model:?} has more than one price"));
            }
        }
        Ok(Prices(prices))
    }

    /// What `input_tokens` cost on `model`, or `None` if the model has no price.
    ///
    /// Rounded up to whole nano-euros, so many small calls never add up to less than they cost.
    pub fn cost(&self, model: &str, input_tokens: u64) -> Option<Eur> {
        // Prices are never negative: they are parsed from unsigned decimals.
        let per_million = self.0.get(model)?.nano() as u128;
        let nano = (per_million * input_tokens as u128).div_ceil(1_000_000);
        Some(Eur(i64::try_from(nano).unwrap_or(i64::MAX)))
    }

    pub fn offers(&self, model: &str) -> bool {
        self.0.contains_key(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_amounts_exactly() {
        assert_eq!("10".parse::<Eur>().unwrap().nano(), 10_000_000_000);
        assert_eq!("0.1".parse::<Eur>().unwrap().nano(), 100_000_000);
        assert_eq!(".5".parse::<Eur>().unwrap().nano(), 500_000_000);
        assert_eq!("2.000000001".parse::<Eur>().unwrap().nano(), 2_000_000_001);
        for bad in [
            "",
            ".",
            "-1",
            "1,5",
            "abc",
            "1.0000000001",
            "99999999999999",
        ] {
            assert!(bad.parse::<Eur>().is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn displays_amounts() {
        assert_eq!(Eur::from_nano(10_000_000_000).to_string(), "10.00 EUR");
        assert_eq!(Eur::from_nano(1_500_000_000).to_string(), "1.50 EUR");
        assert_eq!(Eur::from_nano(100).to_string(), "0.0000001 EUR");
    }

    #[test]
    fn costs_round_up_to_whole_nano_euros() {
        let prices = Prices::parse(&["mistral-embed=0.1", "pricey=0.000001"]).unwrap();
        assert_eq!(
            prices.cost("mistral-embed", 1_000_000),
            Some(Eur(100_000_000))
        );
        assert_eq!(prices.cost("mistral-embed", 3), Some(Eur(300)));
        // 1 nano-euro per million tokens: a single token still costs one nano-euro.
        assert_eq!(prices.cost("pricey", 1), Some(Eur(1)));
        assert_eq!(prices.cost("mistral-embed", 0), Some(Eur::ZERO));
        assert_eq!(prices.cost("unknown", 1), None);
    }

    #[test]
    fn rejects_bad_prices() {
        assert!(Prices::parse(&["mistral-embed"]).is_err());
        assert!(Prices::parse(&["=0.1"]).is_err());
        assert!(Prices::parse(&["a=0.1", "a=0.2"]).is_err());
        assert!(Prices::parse(&["a=cheap"]).is_err());
    }
}
