use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::Error;

/// Nonnegative USD in billionths. No binary floating point enters budget arithmetic.
/// Decimal parsing is exact; excess precision is rejected rather than rounded away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct Money(i64);

impl Money {
    pub const ZERO: Self = Self(0);

    pub fn from_nanos(nanos: i64) -> Result<Self, Error> {
        if nanos < 0 {
            return Err(Error::Invalid("money must be nonnegative"));
        }
        Ok(Self(nanos))
    }

    pub fn nanos(self) -> i64 {
        self.0
    }

    pub fn checked_add(self, other: Self) -> Result<Self, Error> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(Error::Invalid("money overflow"))
    }
}

impl TryFrom<i64> for Money {
    type Error = Error;
    fn try_from(value: i64) -> Result<Self, Error> {
        Self::from_nanos(value)
    }
}

impl From<Money> for i64 {
    fn from(value: Money) -> Self {
        value.nanos()
    }
}

impl FromStr for Money {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        let invalid =
            || Error::Invalid("expected nonnegative USD with at most nine decimal places");
        let (whole, fractional) = value.split_once('.').unwrap_or((value, ""));
        if whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || fractional.len() > 9
            || !fractional.bytes().all(|byte| byte.is_ascii_digit())
            || (value.contains('.') && fractional.is_empty())
        {
            return Err(invalid());
        }
        let whole = whole.parse::<i64>().map_err(|_| invalid())?;
        let fraction = if fractional.is_empty() {
            0
        } else {
            fractional.parse::<i64>().map_err(|_| invalid())?
                * 10_i64.pow(9 - fractional.len() as u32)
        };
        whole
            .checked_mul(1_000_000_000)
            .and_then(|whole| whole.checked_add(fraction))
            .map(Self)
            .ok_or_else(invalid)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{:09}",
            self.0 / 1_000_000_000,
            self.0 % 1_000_000_000
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_decimal_boundaries_round_trip() {
        // Given: zero, the smallest unit and the maximum representable balance.
        // When: parsing USD and formatting the stored integer.
        // Then: no precision is lost.
        for (input, nanos) in [
            ("0", 0),
            ("0.000000001", 1),
            ("1.1", 1_100_000_000),
            ("9223372036.854775807", i64::MAX),
        ] {
            let money: Money = input.parse().unwrap();
            assert_eq!(money.nanos(), nanos);
            assert_eq!(money.to_string().parse::<Money>().unwrap(), money);
        }
    }

    #[test]
    fn invalid_precision_format_sign_and_overflow_are_rejected() {
        // Given: malformed, inexact and out-of-range monetary inputs.
        // When: parsing without implicit trimming or rounding.
        // Then: each value is rejected with a stable error category.
        for value in [
            "",
            "-1",
            "+1",
            " 1",
            "1 ",
            ".1",
            "1.",
            "1e2",
            "NaN",
            "1.2.3",
            "0.0000000001",
            "9223372036.854775808",
            "99999999999999999999",
        ] {
            assert!(
                matches!(value.parse::<Money>(), Err(Error::Invalid(_))),
                "{value}"
            );
        }
    }

    #[test]
    fn negative_json_and_arithmetic_overflow_are_rejected() {
        // Given: invalid integer JSON and a full balance.
        // When: decoding or adding one unit.
        // Then: neither wraps nor constructs a negative Money.
        assert!(serde_json::from_str::<Money>("-1").is_err());
        assert!(serde_json::from_str::<Money>("1.5").is_err());
        assert_eq!(
            Money(i64::MAX).checked_add(Money(1)),
            Err(Error::Invalid("money overflow"))
        );
    }
}
