//! Hash keys, `<site>/<kind>/<parts>`, and the directories they are kept in, see the design's IDs and layout.
//!
//! Each part of a hash key is either an ID, see id.rs, which is a directory named by its hex, or a readable name, of
//! lowercase letters, digits, `.` and `-`, at most 42 characters, so that it can never be taken for an ID, which is 43,
//! and which is a directory of the same name. A sort key decides the file within the DataSet's directory: a day's file,
//! `YYYY/MM/DD.jsonl`, if it starts with a date, as a series' do, and `records.jsonl` otherwise.

use std::path::{Path, PathBuf};

use anyhow::bail;

use crate::id::{Id, is_id};

/// The longest a readable part of a hash key may be, one less than an ID, see the module documentation.
const MAX_NAME: usize = 42;

/// A part of a hash key.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Id(Id),
    Name(String),
}

impl From<Id> for Part {
    fn from(id: Id) -> Part {
        Part::Id(id)
    }
}

impl From<&Id> for Part {
    fn from(id: &Id) -> Part {
        Part::Id(id.clone())
    }
}

impl From<&str> for Part {
    fn from(name: &str) -> Part {
        Part::Name(name.to_string())
    }
}

/// Whether text may be a readable part of a hash key, see the module documentation.
pub fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_NAME
        && text != "."
        && text != ".."
        && text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-')
}

/// The hash key of a DataSet of the kind, e.g. `climate.period`, in site, and its parts after the kind, checked.
pub fn hash_key(site: &Id, kind: &str, parts: &[Part]) -> anyhow::Result<String> {
    if !is_name(kind) {
        bail!("{kind} is not a kind: lowercase letters, digits, . and - only");
    }
    let mut key = format!("{site}/{kind}");
    for part in parts {
        match part {
            Part::Id(id) => key.push_str(&format!("/{id}")),
            Part::Name(name) if is_name(name) => key.push_str(&format!("/{name}")),
            Part::Name(name) => {
                bail!("{name} is not a part of a hash key: an ID, or lowercase letters, digits, . and -")
            }
        }
    }
    Ok(key)
}

/// The site of a hash key, its first part.
pub fn site_of(hk: &str) -> anyhow::Result<Id> {
    Id::parse(hk.split('/').next().unwrap_or_default())
}

/// The directory of the DataSet hk, under the site's directory, each part after the site a directory, an ID by its hex.
pub fn data_set_dir(site_dir: &Path, hk: &str) -> anyhow::Result<PathBuf> {
    let mut parts = hk.split('/');
    parts.next();
    let mut dir = site_dir.to_path_buf();
    let mut any = false;
    for part in parts {
        any = true;
        if is_id(part) {
            dir.push(Id::parse(part)?.hex());
        } else if is_name(part) {
            dir.push(part);
        } else {
            bail!("{hk} is not a hash key: {part} is neither an ID nor a name");
        }
    }
    if !any {
        bail!("{hk} is not a hash key: it has no kind");
    }
    Ok(dir)
}

/// The date a sort key starts with, as (year, month, day) text, if it does, e.g. a series' record's time.
pub fn sort_key_date(sk: &str) -> Option<(&str, &str, &str)> {
    let bytes = sk.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        range
            .into_iter()
            .all(|index| bytes.get(index).is_some_and(u8::is_ascii_digit))
    };
    if digits(0..4) && bytes.get(4) == Some(&b'-') && digits(5..7) && bytes.get(7) == Some(&b'-') && digits(8..10) {
        Some((&sk[0..4], &sk[5..7], &sk[8..10]))
    } else {
        None
    }
}

/// The file, under its DataSet's directory, of the record with the sort key sk, see the module documentation.
pub fn file_of(sk: &str) -> PathBuf {
    match sort_key_date(sk) {
        Some((year, month, day)) => PathBuf::from(year).join(month).join(format!("{day}.jsonl")),
        None => PathBuf::from(RECORDS),
    }
}

/// The file of a DataSet's records whose sort keys do not start with a date.
pub const RECORDS: &str = "records.jsonl";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn site() -> Id {
        Id::of(&json!({"site": "test"})).unwrap()
    }

    #[test]
    fn a_hash_key_is_its_site_kind_and_parts_and_ids_are_hex_directories() {
        let meter = Id::of(&json!({"meter": "1"})).unwrap();
        let hk = hash_key(&site(), "octopus.reading", &[Part::from(&meter), Part::from("import")]).unwrap();
        assert_eq!(hk, format!("{}/octopus.reading/{meter}/import", site()));
        let dir = data_set_dir(Path::new("/root"), &hk).unwrap();
        assert_eq!(
            dir,
            Path::new("/root")
                .join("octopus.reading")
                .join(meter.hex())
                .join("import")
        );
        assert_eq!(site_of(&hk).unwrap(), site());
    }

    #[test]
    fn names_which_could_be_ids_or_escape_the_directory_are_refused() {
        assert!(hash_key(&site(), "Climate", &[]).is_err());
        assert!(hash_key(&site(), "climate", &[Part::from("..")]).is_err());
        assert!(hash_key(&site(), "climate", &[Part::from("a/b")]).is_err());
        assert!(hash_key(&site(), "climate", &[Part::Name("a".repeat(43))]).is_err());
        assert!(data_set_dir(Path::new("/root"), &site().to_string()).is_err());
    }

    #[test]
    fn a_sort_key_starting_with_a_date_is_kept_in_that_days_file() {
        assert_eq!(file_of("2026-11-04T07:17:10Z"), PathBuf::from("2026/11/04.jsonl"));
        assert_eq!(file_of("2026-10-01#bill-1"), PathBuf::from("2026/10/01.jsonl"));
        assert_eq!(file_of("2026-11"), PathBuf::from(RECORDS));
        assert_eq!(file_of("#fetched/2026-11"), PathBuf::from(RECORDS));
        assert_eq!(file_of("-"), PathBuf::from(RECORDS));
    }
}
