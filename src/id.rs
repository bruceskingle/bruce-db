//! IDs: the SHA-256 of the canonical JSON (RFC 8785) of what an ID is made from, in URL-safe base64 without padding,
//! 43 characters, and in hex, 64 characters, in file names, see the design's IDs.
//!
//! What an ID is made from must follow the design's rules, which are enforced here: no arrays, numbers which are exact
//! as IEEE doubles, and strings normalised to Unicode NFC. Normalising needs Unicode's tables, which are not in the
//! crate yet, so for now a string must be ASCII, which is always NFC, and any other is refused rather than hashed
//! unnormalised.

use anyhow::{anyhow, bail};
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The largest integer every IEEE double holds exactly, 2^53.
const MAX_EXACT: f64 = 9_007_199_254_740_992.0;

/// An ID, see the module documentation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(String);

impl Id {
    /// The ID of value, which must follow the rules, see check.
    pub fn of(value: &Value) -> anyhow::Result<Id> {
        check(value, "")?;
        let canonical =
            serde_jcs::to_string(value).map_err(|error| anyhow!("Unable to make canonical JSON, {error}"))?;
        Ok(Id::of_bytes(canonical.as_bytes()))
    }

    /// The ID of bytes, e.g. a blob's contents, see blob.rs.
    pub fn of_bytes(bytes: &[u8]) -> Id {
        Id(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
    }

    /// An ID as text, checked to be one.
    pub fn parse(text: &str) -> anyhow::Result<Id> {
        if !is_id(text) {
            bail!("{text} is not an ID");
        }
        Ok(Id(text.to_string()))
    }

    /// From its hex, e.g. a file name.
    pub fn from_hex(hex: &str) -> anyhow::Result<Id> {
        if hex.len() != 64 {
            bail!("{hex} is not the hex of an ID");
        }
        let bytes = (0..32)
            .map(|index| u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| anyhow!("{hex} is not the hex of an ID"))?;
        Ok(Id(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The 64 lowercase hex characters of the same hash, for file names, as file systems which ignore case would take
    /// two base64 IDs for one.
    pub fn hex(&self) -> String {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&self.0)
            .unwrap_or_default();
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether text is an ID: 43 characters of URL-safe base64 which decode to 32 bytes.
pub fn is_id(text: &str) -> bool {
    text.len() == 43
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        && base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(text)
            .is_ok_and(|bytes| bytes.len() == 32)
}

/// Checks that value follows the rules for what an ID is made from, saying where it does not, at path.
fn check(value: &Value, path: &str) -> anyhow::Result<()> {
    match value {
        Value::Array(_) => bail!(
            "An ID can not be made from an array, at {}",
            if path.is_empty() { "the top" } else { path }
        ),
        Value::Object(fields) => {
            for (name, field) in fields {
                check_string(name, path)?;
                check(field, &format!("{path}/{name}"))?;
            }
            Ok(())
        }
        Value::String(text) => check_string(text, path),
        Value::Number(number) => {
            let exact = match (number.as_i64(), number.as_u64(), number.as_f64()) {
                // Compared as integers, as converting one past the limit to a double rounds it down to the limit.
                (Some(integer), _, _) => integer.unsigned_abs() <= MAX_EXACT as u64,
                (_, Some(integer), _) => integer <= MAX_EXACT as u64,
                (_, _, Some(float)) => float.is_finite(),
                _ => false,
            };
            if !exact {
                bail!(
                    "An ID can not be made from {number}, at {path}, which is not exact as a double: make it a string"
                );
            }
            Ok(())
        }
        Value::Bool(_) | Value::Null => Ok(()),
    }
}

/// See the module documentation: only ASCII until strings can be normalised to NFC.
fn check_string(text: &str, path: &str) -> anyhow::Result<()> {
    if !text.is_ascii() {
        bail!(
            "An ID can not yet be made from text which is not ASCII, at {path}, as it can not be normalised to NFC yet"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_same_fields_in_any_order_give_the_same_id_and_other_fields_another() {
        let one = Id::of(&json!({"account": "A-1", "meter": "123"})).unwrap();
        let same = Id::of(&json!({"meter": "123", "account": "A-1"})).unwrap();
        let other = Id::of(&json!({"account": "A-1", "meter": "124"})).unwrap();
        assert_eq!(one, same);
        assert_ne!(one, other);
        assert_eq!(one.as_str().len(), 43);
        assert!(is_id(one.as_str()));
    }

    #[test]
    fn an_id_is_the_sha256_of_its_canonical_json() {
        // The SHA-256 of the canonical JSON {"a":1}.
        let id = Id::of(&json!({"a": 1})).unwrap();
        let expected: String = Sha256::digest(br#"{"a":1}"#)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(id.hex(), expected);
        assert_eq!(Id::from_hex(&id.hex()).unwrap(), id);
    }

    #[test]
    fn arrays_inexact_numbers_and_text_not_yet_normalised_are_refused() {
        assert!(Id::of(&json!({"a": [1, 2]})).is_err());
        assert!(Id::of(&json!({"a": 9_007_199_254_740_993u64})).is_err());
        assert!(Id::of(&json!({"name": "Zoë"})).is_err());
        assert!(Id::of(&json!({"a": 1.5, "b": true, "c": null})).is_ok());
    }
}
