//! A record, its version, and the times they are written with, see the design's record model.

use std::cmp::Ordering;

use anyhow::{Context, anyhow};
use serde_json::Value;
use time::OffsetDateTime;

/// A time, in microseconds since the Unix epoch, UTC, which is how versions and the times records were stored are
/// kept, and written as RFC 3339 with exactly six digits of fraction, so that text order is time order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Micros(pub i64);

const FORMAT: &[time::format_description::FormatItem<'static>] =
    time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z");

impl Micros {
    /// The time now, from this node's clock.
    pub fn now() -> Micros {
        Micros((OffsetDateTime::now_utc().unix_timestamp_nanos() / 1000) as i64)
    }

    pub fn from_datetime(time: OffsetDateTime) -> Micros {
        Micros((time.unix_timestamp_nanos() / 1000) as i64)
    }

    pub fn to_datetime(self) -> anyhow::Result<OffsetDateTime> {
        Ok(OffsetDateTime::from_unix_timestamp_nanos(self.0 as i128 * 1000)?)
    }

    /// The UTC day it is in, e.g. for the journal file it is written to.
    pub fn date(self) -> anyhow::Result<time::Date> {
        Ok(self.to_datetime()?.date())
    }

    /// As RFC 3339, e.g. 2026-11-04T07:20:30.000000Z.
    pub fn to_text(self) -> String {
        self.to_datetime()
            .ok()
            .and_then(|time| time.format(FORMAT).ok())
            .unwrap_or_default()
    }

    /// From RFC 3339, with any fraction, or none, and any offset.
    pub fn parse(text: &str) -> anyhow::Result<Micros> {
        let time = OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
            .with_context(|| format!("{text} is not an RFC 3339 time"))?;
        Ok(Micros::from_datetime(time))
    }
}

/// Which copy of a record is newest: its time, from the clock of the node which wrote it, then, of equal times, the
/// lowest origin, the ID of the node which wrote it, so that every node chooses the same copy whatever order they
/// arrive in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub time: Micros,
    pub origin: String,
}

impl Version {
    pub fn new(time: Micros, origin: &str) -> Version {
        Version {
            time,
            origin: origin.to_string(),
        }
    }
}

impl Ord for Version {
    /// Later is newer, and of equal times the lower origin is newer, so that it wins.
    fn cmp(&self, other: &Self) -> Ordering {
        self.time.cmp(&other.time).then_with(|| other.origin.cmp(&self.origin))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A record: where it is, its hash key, the DataSet, and its sort key, its version, and its attributes, None for a
/// tombstone, which says the record has been deleted.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub hk: String,
    pub sk: String,
    pub version: Version,
    pub attributes: Option<Value>,
}

impl Item {
    pub fn new(hk: &str, sk: &str, version: Version, attributes: Value) -> Item {
        Item {
            hk: hk.to_string(),
            sk: sk.to_string(),
            version,
            attributes: Some(attributes),
        }
    }

    pub fn tombstone(hk: &str, sk: &str, version: Version) -> Item {
        Item {
            hk: hk.to_string(),
            sk: sk.to_string(),
            version,
            attributes: None,
        }
    }

    pub fn is_tombstone(&self) -> bool {
        self.attributes.is_none()
    }
}

/// A record as a line of a journal or a compacted file: the record, its sequence number in this node's store, `n`,
/// which orders the change feed and is the replication cursor, when this node stored it, `t`, for people to read, and
/// the peer it was pulled from, `from`, None if it was put here, see the design's replication.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Line {
    pub n: u64,
    pub t: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub o: String,
    pub hk: String,
    pub sk: String,
    pub v: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<Value>,
}

impl Line {
    pub fn new(item: &Item, sequence: u64, stored: Micros, from: Option<&str>) -> Line {
        Line {
            n: sequence,
            t: stored.to_text(),
            from: from.map(str::to_string),
            o: item.version.origin.clone(),
            hk: item.hk.clone(),
            sk: item.sk.clone(),
            v: item.version.time.to_text(),
            a: item.attributes.clone(),
        }
    }

    pub fn item(&self) -> anyhow::Result<Item> {
        Ok(Item {
            hk: self.hk.clone(),
            sk: self.sk.clone(),
            version: Version::new(Micros::parse(&self.v)?, &self.o),
            attributes: self.a.clone(),
        })
    }

    pub fn version(&self) -> anyhow::Result<Version> {
        Ok(Version::new(Micros::parse(&self.v)?, &self.o))
    }

    pub fn stored(&self) -> anyhow::Result<Micros> {
        Micros::parse(&self.t)
    }

    pub fn parse(text: &str) -> anyhow::Result<Line> {
        serde_json::from_str(text).map_err(|error| anyhow!("A line of the store is not a record, {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_version_is_newer_and_of_equal_times_the_lowest_origin_is() {
        let early = Version::new(Micros(1), "b");
        let late = Version::new(Micros(2), "b");
        assert!(late > early);
        assert!(Version::new(Micros(1), "a") > Version::new(Micros(1), "b"));
    }

    #[test]
    fn times_are_written_with_six_digits_so_that_text_order_is_time_order() {
        let time = Micros::parse("2026-11-04T07:20:30Z").unwrap();
        assert_eq!(time.to_text(), "2026-11-04T07:20:30.000000Z");
        assert_eq!(Micros::parse(&time.to_text()).unwrap(), time);
        assert!(Micros(time.0 + 1).to_text() > time.to_text());
        assert_eq!(Micros::parse("2026-11-04T17:20:30+10:00").unwrap(), time);
    }
}
