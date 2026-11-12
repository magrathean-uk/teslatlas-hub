// SPDX-License-Identifier: AGPL-3.0-only

//! Single-pass encoding of a row struct into the canonical JSON object bytes
//! the importer persists.
//!
//! The stage and the projection-state spool store every row as the bytes
//! `serde_json::to_string(&serde_json::to_value(row)?)` produces: one JSON
//! object whose top-level keys are in byte order, because `serde_json::Value`
//! keeps object keys in a `BTreeMap` (the `preserve_order` feature is off).
//! The `source_db_canonical_json_*` tests are the guard that `preserve_order` stays off.
//! Building the whole intermediate `Value` costs a `BTreeMap` node and an owned
//! key `String` per field for every one of millions of rows.
//!
//! [`encode_sorted_object`] produces the same bytes without that intermediate
//! object: it collects each top-level field's own `Value` beside its static
//! name, sorts the names in the same byte order, and writes the object once.
//! Field values still go through `serde_json::to_value`, so nested objects keep
//! their sorted keys and every scalar is formatted exactly as before. Any shape
//! other than a plain struct (maps, flattened structs, scalars, serde_json's
//! private raw-value marker, duplicate names or any serialisation error)
//! returns `None`, and the caller falls back to the two-pass encoding, which
//! also reproduces the original error for rejected rows.

use std::fmt;

use serde::Serialize;
use serde::ser::{self, Impossible, SerializeStruct};
use serde_json::Value;

/// Encode `value` as a canonical JSON object in one pass, or return `None`
/// when only the two-pass path can guarantee identical bytes.
pub(crate) fn encode_sorted_object<T: Serialize + ?Sized>(value: &T) -> Option<Vec<u8>> {
    let mut fields = value.serialize(TopLevel).ok()?;
    // Byte order of `&str` equals the `Ord` of the `String` keys a `Value`
    // object uses, so the written order matches the two-pass encoding.
    fields.sort_by(|left, right| left.0.cmp(right.0));
    if fields.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return None;
    }
    let mut out = Vec::with_capacity(fields.len() * 24 + 2);
    out.push(b'{');
    for (index, (key, field)) in fields.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        serde_json::to_writer(&mut out, key).ok()?;
        out.push(b':');
        serde_json::to_writer(&mut out, field).ok()?;
    }
    out.push(b'}');
    Some(out)
}

/// Same as [`encode_sorted_object`], as a `String`.
pub(crate) fn encode_sorted_object_string<T: Serialize + ?Sized>(value: &T) -> Option<String> {
    String::from_utf8(encode_sorted_object(value)?).ok()
}

type Fields = Vec<(&'static str, Value)>;

#[derive(Debug)]
struct Unsupported;

impl fmt::Display for Unsupported {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("shape needs the two-pass canonical encoding")
    }
}

impl std::error::Error for Unsupported {}

impl ser::Error for Unsupported {
    fn custom<M: fmt::Display>(_message: M) -> Self {
        Self
    }
}

struct TopLevel;

struct StructFields(Fields);

impl SerializeStruct for StructFields {
    type Ok = Fields;
    type Error = Unsupported;

    fn serialize_field<F: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &F,
    ) -> Result<(), Unsupported> {
        let value = serde_json::to_value(value).map_err(|_| Unsupported)?;
        self.0.push((key, value));
        Ok(())
    }

    fn end(self) -> Result<Fields, Unsupported> {
        Ok(self.0)
    }
}

impl ser::Serializer for TopLevel {
    type Ok = Fields;
    type Error = Unsupported;
    type SerializeSeq = Impossible<Fields, Unsupported>;
    type SerializeTuple = Impossible<Fields, Unsupported>;
    type SerializeTupleStruct = Impossible<Fields, Unsupported>;
    type SerializeTupleVariant = Impossible<Fields, Unsupported>;
    type SerializeMap = Impossible<Fields, Unsupported>;
    type SerializeStruct = StructFields;
    type SerializeStructVariant = Impossible<Fields, Unsupported>;

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<StructFields, Unsupported> {
        // serde_json reserves `$serde_json::...` struct names for its private
        // raw-value and number markers; leave them to serde_json itself.
        if name.starts_with("$serde_json") {
            return Err(Unsupported);
        }
        Ok(StructFields(Vec::with_capacity(len)))
    }

    fn serialize_newtype_struct<F: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &F,
    ) -> Result<Fields, Unsupported> {
        // serde_json's value serializer is transparent for newtype structs.
        value.serialize(self)
    }

    fn serialize_bool(self, _: bool) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_i8(self, _: i8) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_i16(self, _: i16) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_i32(self, _: i32) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_i64(self, _: i64) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_u8(self, _: u8) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_u16(self, _: u16) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_u32(self, _: u32) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_u64(self, _: u64) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_f32(self, _: f32) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_f64(self, _: f64) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_char(self, _: char) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_str(self, _: &str) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_none(self) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_some<F: ?Sized + Serialize>(self, _: &F) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_unit(self) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_newtype_variant<F: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &F,
    ) -> Result<Fields, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Unsupported> {
        Err(Unsupported)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Unsupported> {
        Err(Unsupported)
    }
}

#[cfg(test)]
pub(crate) mod tests;
