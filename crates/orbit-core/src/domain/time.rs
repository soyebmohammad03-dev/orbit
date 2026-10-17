//! UTC timestamps with microsecond precision.

use std::fmt;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ValidationError;

/// A UTC instant with microsecond precision, stored as microseconds since
/// the Unix epoch.
///
/// Timestamps are informational. ORBIT never orders events by timestamp:
/// wall clocks can jump backwards, and two events may share a timestamp.
/// The supported range is 1970-01-01T00:00:00Z to 9999-12-31T23:59:59.999999Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// Largest supported value: 9999-12-31T23:59:59.999999Z.
    const MAX_MICROS: i64 = 253_402_300_799_999_999;

    /// Reads the system clock, truncating to whole microseconds.
    ///
    /// # Errors
    /// Returns [`ValidationError::TimestampOutOfRange`] if the system clock
    /// reports an instant outside the supported range (e.g. before 1970).
    pub fn now() -> Result<Self, ValidationError> {
        Self::from_unix_micros(Utc::now().timestamp_micros())
    }

    /// Wraps microseconds since the Unix epoch.
    ///
    /// # Errors
    /// Returns [`ValidationError::TimestampOutOfRange`] outside the supported range.
    pub fn from_unix_micros(micros: i64) -> Result<Self, ValidationError> {
        if (0..=Self::MAX_MICROS).contains(&micros) {
            Ok(Self(micros))
        } else {
            Err(ValidationError::TimestampOutOfRange { micros })
        }
    }

    /// Microseconds since the Unix epoch.
    #[must_use]
    pub fn unix_micros(self) -> i64 {
        self.0
    }

    /// Converts to a `chrono` UTC date-time.
    #[must_use]
    pub fn to_datetime(self) -> DateTime<Utc> {
        // In range by construction, so the fallback is unreachable.
        DateTime::from_timestamp_micros(self.0).unwrap_or_default()
    }

    /// RFC 3339 rendering with exactly six fractional digits and a `Z` suffix.
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        self.to_datetime()
            .to_rfc3339_opts(SecondsFormat::Micros, true)
    }

    /// Parses an RFC 3339 timestamp. Sub-microsecond digits are rejected
    /// rather than silently truncated.
    ///
    /// # Errors
    /// Returns a [`ValidationError`] for malformed or out-of-range input.
    pub fn parse_rfc3339(text: &str) -> Result<Self, ValidationError> {
        let parsed = DateTime::parse_from_rfc3339(text)
            .map_err(|e| ValidationError::InvalidTimestamp(e.to_string()))?
            .with_timezone(&Utc);
        if parsed.timestamp_subsec_nanos() % 1_000 != 0 {
            return Err(ValidationError::InvalidTimestamp(
                "precision finer than one microsecond".to_owned(),
            ));
        }
        Self::from_unix_micros(parsed.timestamp_micros())
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_rfc3339())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse_rfc3339(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_is_enforced() {
        assert!(Timestamp::from_unix_micros(0).is_ok());
        assert!(Timestamp::from_unix_micros(Timestamp::MAX_MICROS).is_ok());
        assert!(Timestamp::from_unix_micros(-1).is_err());
        assert!(Timestamp::from_unix_micros(Timestamp::MAX_MICROS + 1).is_err());
        assert!(Timestamp::now().is_ok());
    }

    #[test]
    fn rfc3339_round_trip() {
        let ts = Timestamp::from_unix_micros(1_700_000_000_123_456).unwrap();
        assert_eq!(ts.to_rfc3339(), "2023-11-14T22:13:20.123456Z");
        assert_eq!(Timestamp::parse_rfc3339(&ts.to_rfc3339()).unwrap(), ts);
        assert_eq!(
            Timestamp::parse_rfc3339("2023-11-14T23:13:20.123456+01:00").unwrap(),
            ts
        );
        assert!(Timestamp::parse_rfc3339("2023-11-14T22:13:20.1234567Z").is_err());
        assert!(Timestamp::parse_rfc3339("yesterday").is_err());
        assert!(Timestamp::parse_rfc3339("1969-12-31T23:59:59Z").is_err());
        let json = serde_json::to_string(&ts).unwrap();
        assert_eq!(json, "\"2023-11-14T22:13:20.123456Z\"");
        assert_eq!(serde_json::from_str::<Timestamp>(&json).unwrap(), ts);
    }
}
