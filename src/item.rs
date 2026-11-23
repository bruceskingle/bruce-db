//! A record, its version, and the times they are written with, see the design's record model.

use std::cmp::Ordering;

use anyhow::{Context, anyhow, bail};
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

    /// The line as it is written, without its newline: its JSON, sealed with its checksum, see seal.
    pub fn to_text(&self) -> anyhow::Result<String> {
        let mut text = serde_json::to_string(self)?;
        seal(&mut text);
        Ok(text)
    }
}

/// How a line's checksum starts, after the rest of its JSON, see seal: `,"c":"`, then 8 hex digits and `"}`.
const CHECKSUM_START: &str = ",\"c\":\"";
/// The length of a line's checksum field and the brace closing the line, `,"c":"0123abcd"}`.
const CHECKSUM_LENGTH: usize = CHECKSUM_START.len() + 8 + 2;

/// Adds a CRC32 of the line's JSON, text, as its last field, "c", in hex, so that a line damaged on the disk, e.g. with
/// a bit flipped by a failing SD card, which may still be JSON, e.g. with a digit changed, is found when it is read,
/// see check. The checksum is of the JSON as it was before it was added, so that checking it needs no serialising
/// again, only the text without it.
pub(crate) fn seal(text: &mut String) {
    let checksum = crc32(&[text.as_bytes()]);
    text.pop();
    text.push_str(&format!("{CHECKSUM_START}{checksum:08x}\"}}"));
}

/// Whether a line's text, without its newline, is as it was written, by its checksum, see seal. A line without one,
/// written before lines had them, is taken as it is; one whose checksum is itself damaged is not.
pub(crate) fn check(text: &str) -> anyhow::Result<()> {
    let length = text.len();
    let sealed = length > CHECKSUM_LENGTH
        && text.is_char_boundary(length - CHECKSUM_LENGTH)
        && text[length - CHECKSUM_LENGTH..].starts_with(CHECKSUM_START)
        && text.ends_with("\"}");
    if !sealed {
        // A checksum field near the end, but not as one is written, is a damaged one.
        let tail = &text.as_bytes()[length.saturating_sub(CHECKSUM_LENGTH + 4)..];
        if tail.windows(4).any(|window| window == b"\"c\":") {
            bail!("The line's checksum is damaged");
        }
        return Ok(());
    }
    let hex = &text[length - 10..length - 2];
    let written = u32::from_str_radix(hex, 16).map_err(|_| anyhow!("The line's checksum, {hex}, is damaged"))?;
    let json = &text[..length - CHECKSUM_LENGTH];
    let actual = crc32(&[json.as_bytes(), b"}"]);
    if actual != written {
        bail!("The line is damaged: its checksum is {written:08x}, but its text's is {actual:08x}");
    }
    Ok(())
}

/// The CRC32 (IEEE, as zip's and Ethernet's) of parts, one after another, a byte at a time with CRC_TABLE, as every line
/// read is checked, which a bit at a time made a noticeable part of reading a day of the Home Mini's readings.
fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for byte in parts.iter().flat_map(|part| part.iter()) {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(*byte)) & 0xff) as usize];
    }
    !crc
}

/// The CRC32 of each byte, for crc32, worked out when compiled.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
};

/// A line's keys, sequence number and version without its attributes, which is all an index of the lines needs, read
/// without building the attributes' JSON, which is most of a line, e.g. a Tesla snapshot's responses.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub(crate) struct Head {
    pub n: u64,
    pub o: String,
    pub hk: String,
    pub sk: String,
    pub v: String,
}

impl Head {
    pub fn parse(text: &str) -> anyhow::Result<Head> {
        serde_json::from_str(text).map_err(|error| anyhow!("A line of the store is not a record, {error}"))
    }

    pub fn of(line: &Line) -> Head {
        Head {
            n: line.n,
            o: line.o.clone(),
            hk: line.hk.clone(),
            sk: line.sk.clone(),
            v: line.v.clone(),
        }
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

    #[test]
    fn the_checksum_is_the_standard_crc32() {
        assert_eq!(crc32(&[b"123456789"]), 0xcbf4_3926);
        assert_eq!(crc32(&[b"1234", b"56789"]), 0xcbf4_3926);
    }

    #[test]
    fn a_sealed_line_is_checked_and_one_changed_is_refused() {
        let item = Item {
            hk: "site/tesla.snapshot/gw".to_string(),
            sk: "2026-11-23T04:42:10Z".to_string(),
            version: Version::new(Micros(1), "dev"),
            attributes: Some(serde_json::json!({"power": 1234})),
        };
        let text = Line::new(&item, 7, Micros(2), None).to_text().unwrap();
        assert!(text.ends_with("\"}"));
        check(&text).unwrap();
        assert_eq!(
            Line::parse(&text).unwrap().a,
            item.attributes,
            "the checksum field is ignored when parsed"
        );
        let changed = text.replace("1234", "1236");
        assert!(Line::parse(&changed).is_ok(), "still JSON");
        assert!(check(&changed).is_err(), "but refused");
        let unsealed = serde_json::to_string(&Line::new(&item, 7, Micros(2), None)).unwrap();
        check(&unsealed).expect("a line from before checksums is taken as it is");
        let damaged = text.replacen("\"c\":\"", "\"c\":\"x", 1);
        assert!(check(&damaged).is_err(), "a damaged checksum is not taken for none");
    }
}
