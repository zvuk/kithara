use std::{
    fmt,
    io::{self, Write},
};

use serde::{
    Serialize,
    de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{MAX_VIEW_BYTES, PlanViewV3, ReadPins, require};

struct Ascii;

impl serde_json::ser::Formatter for Ascii {
    fn write_string_fragment<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        for character in fragment.chars() {
            if character <= '\u{7e}' {
                writer.write_all(&[character as u8])?;
            } else {
                let mut units = [0; 2];
                for unit in character.encode_utf16(&mut units) {
                    write!(writer, "\\u{:04x}", *unit)?;
                }
            }
        }
        Ok(())
    }
}

/// Serialize `value` as canonical view bytes: sorted keys, compact, ASCII escapes.
///
/// # Errors
///
/// Fails when `value` has no JSON representation.
pub fn canonical_json<T: Serialize + ?Sized>(value: &T) -> io::Result<Vec<u8>> {
    let mut value = serde_json::to_value(value).map_err(io::Error::other)?;
    value.sort_all_objects();
    let mut bytes = Vec::new();
    value
        .serialize(&mut serde_json::Serializer::with_formatter(
            &mut bytes, Ascii,
        ))
        .map_err(io::Error::other)?;
    Ok(bytes)
}

/// SHA-256 of the canonical bytes of `value`, in lowercase hex.
///
/// # Errors
///
/// Fails when `value` has no JSON representation.
pub fn digest<T: Serialize + ?Sized>(value: &T) -> io::Result<String> {
    Ok(hex::encode(Sha256::digest(canonical_json(value)?)))
}

/// Decode bounded canonical wire bytes. Publisher authentication belongs to the caller.
///
/// # Errors
///
/// Fails on oversized, non-canonical or mismatching bytes and on a view that
/// violates the fixed shape or the reader pins.
pub fn decode(bytes: &[u8], expected_sha256: &str, pins: &ReadPins<'_>) -> io::Result<PlanViewV3> {
    require(
        !bytes.is_empty() && bytes.len() <= MAX_VIEW_BYTES,
        "view size",
    )?;
    require(
        hex::encode(Sha256::digest(bytes)) == expected_sha256,
        "view byte digest",
    )?;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = Unique(0)
        .deserialize(&mut deserializer)
        .map_err(io::Error::other)?;
    deserializer.end().map_err(io::Error::other)?;
    let view: PlanViewV3 = serde_json::from_value(value).map_err(io::Error::other)?;
    view.validate(pins)?;
    require(canonical_json(&view)? == bytes, "noncanonical view bytes")?;
    Ok(view)
}

struct Unique(usize);

impl<'de> DeserializeSeed<'de> for Unique {
    type Value = Value;

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        if self.0 > 64 {
            return Err(D::Error::custom("view nesting limit"));
        }
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Unique {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("unique-key JSON with unsigned integers")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if key.is_empty()
                || key.len() > 512
                || !key.is_ascii()
                || values.len() >= 10_000
                || values.contains_key(&key)
            {
                return Err(A::Error::custom("invalid or duplicate view key"));
            }
            values.insert(key, map.next_value_seed(Self(self.0 + 1))?);
        }
        Ok(Value::Object(values))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(Self(self.0 + 1))? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
        if value.chars().count() > 65_536 {
            return Err(E::custom("view string limit"));
        }
        Ok(Value::String(value))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        self.visit_string(value.to_owned())
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
}
