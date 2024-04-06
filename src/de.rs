// Copyright 2018 Serde Developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use crate::error::{Error, Result};
use serde::de::{
    self, Deserialize, DeserializeSeed, EnumAccess, IntoDeserializer,
    MapAccess, SeqAccess, VariantAccess, Visitor,
};
use std::{
    io::Read,
    ops::{AddAssign, MulAssign, Neg},
};

pub struct Deserializer<R: Read> {
    // This string starts with the input data and characters are truncated off
    // the beginning as data is parsed.
    reader: R,
}

impl<'a> Deserializer<&'a [u8]> {
    // By convention, `Deserializer` constructors are named like `from_xyz`.
    // That way basic use cases are satisfied by something like
    // `serde_json::from_str(...)` while advanced use cases that require a
    // deserializer can make one with `serde_json::Deserializer::from_str(...)`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_bytes(input: &'a [u8]) -> Self {
        Deserializer { reader: input }
    }
}

// By convention, the public API of a Serde deserializer is one or more
// `from_xyz` methods such as `from_str`, `from_bytes`, or `from_reader`
// depending on what Rust types the deserializer is able to consume as input.
//
// This basic deserializer supports only `from_str`.
pub fn from_bytes<'a, T>(s: &'a [u8]) -> Result<T>
where
    T: Deserialize<'a>,
{
    let mut deserializer = Deserializer::from_bytes(s);
    let t = T::deserialize(&mut deserializer)?;
    if deserializer.reader.is_empty() {
        Ok(t)
    } else {
        Err(Error::TrailingCharacters)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementType {
    Null,
    True,
    False,
    Int,
    Int5,
    Float,
    Float5,
    Text,
    TextJ,
    Text5,
    TextRaw,
    Array,
    Object,
    Reserved13,
    Reserved14,
    Reserved15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    element_type: ElementType,
    payload_size: usize,
}

impl<R: Read> Deserializer<R> {
    fn read_header(&mut self) -> Result<Header> {
        /*  The upper four bits of the first byte of the header determine
          - size of the header
          - and possibly also the size of the payload.
        */
        let mut header_0 = [0u8; 1];
        self.reader.read_exact(&mut header_0)?;
        let first_byte = header_0[0];
        let upper_four_bits = first_byte >> 4;
        /*
         If the upper four bits have a value between 0 and 11,
        then the header is exactly one byte in size and the payload size is determined by those upper four bits.

        If the upper four bits have a value between 12 and 15,
        that means that the total header size is 2, 3, 5, or 9 bytes and the payload size is unsigned big-endian integer that is contained in the subsequent bytes.

        The size integer is
          - the one byte that following the initial header byte if the upper four bits are 12,
          - two bytes if the upper bits are 13,
          - four bytes if the upper bits are 14,
          - and eight bytes if the upper bits are 15.
        */
        let bytes_to_read = match upper_four_bits {
            0..=11 => 0,
            12 => 1,
            13 => 2,
            14 => 4,
            15 => 8,
            n => unreachable!("{n} does not fit in four bits"),
        };
        let payload_size: usize = if bytes_to_read == 0 {
            usize::from(upper_four_bits)
        } else {
            let mut buf = [0u8; 8];
            let start = 8 - bytes_to_read;
            self.reader.read_exact(&mut buf[start..8])?;
            usize::from_be_bytes(buf)
        };
        let lower_four_bits = first_byte & 0x0F;
        let element_type = match lower_four_bits {
            0 => ElementType::Null,
            1 => ElementType::True,
            2 => ElementType::False,
            3 => ElementType::Int,
            4 => ElementType::Int5,
            5 => ElementType::Float,
            6 => ElementType::Float5,
            7 => ElementType::Text,
            8 => ElementType::TextJ,
            9 => ElementType::Text5,
            10 => ElementType::TextRaw,
            11 => ElementType::Array,
            12 => ElementType::Object,
            13 => ElementType::Reserved13,
            14 => ElementType::Reserved14,
            15 => ElementType::Reserved15,
            n => unreachable!("{n} does not fit in four bits"),
        };
        Ok(Header {
            element_type,
            payload_size,
        })
    }

    fn read_header_with_payload(&mut self) -> Result<(ElementType, Vec<u8>)> {
        let header = self.read_header()?;
        let mut buf = vec![0; header.payload_size];
        self.reader.read_exact(&mut buf)?;
        Ok((header.element_type, buf))
    }

    fn drop_payload(&mut self, header: Header) -> Result<ElementType> {
        let mut remaining = header.payload_size;
        while remaining > 0 {
            let mut buf = [0u8; 256];
            let len = buf.len().min(remaining);
            self.reader.read_exact(&mut buf[..len])?;
            remaining -= len;
        }
        Ok(header.element_type)
    }

    fn read_bool(&mut self, header: Header) -> Result<bool> {
        self.drop_payload(header)?;
        match header.element_type {
            ElementType::True => Ok(true),
            ElementType::False => Ok(false),
            t => Err(Error::UnexpectedType(t)),
        }
    }

    fn read_null(&mut self, header: Header) -> Result<()> {
        self.drop_payload(header)?;
        match header.element_type {
            ElementType::Null => Ok(()),
            t => Err(Error::UnexpectedType(t)),
        }
    }

    fn read_json_compatible<T>(&mut self, header: Header) -> Result<T>
    where
        for<'a> T: Deserialize<'a>,
    {
        let limit =
            u64::try_from(header.payload_size).map_err(usize_conversion)?;
        let mut reader = (&mut self.reader).take(limit);
        Ok(crate::json::parse_json(&mut reader)?)
    }

    fn read_json5_compatible<T>(&mut self, header: Header) -> Result<T>
    where
        for<'a> T: Deserialize<'a>,
    {
        let limit =
            u64::try_from(header.payload_size).map_err(usize_conversion)?;
        let mut reader = (&mut self.reader).take(limit);
        Ok(crate::json::parse_json5(&mut reader)?)
    }

    fn read_integer<T>(&mut self, header: Header) -> Result<T>
    where
        for<'a> T: Deserialize<'a>,
    {
        match header.element_type {
            ElementType::Int => self.read_json_compatible(header),
            ElementType::Int5 => self.read_json5_compatible(header),
            t => return Err(Error::UnexpectedType(t)),
        }
    }
}

fn usize_conversion(e: std::num::TryFromIntError) -> Error {
    Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

impl<'de, 'a, R: Read> de::Deserializer<'de> for &'a mut Deserializer<R> {
    type Error = Error;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let header = self.read_header()?;
        match header.element_type {
            ElementType::Null => {
                self.read_null(header)?;
                visitor.visit_unit()
            }
            ElementType::True | ElementType::False => {
                let b = self.read_bool(header)?;
                visitor.visit_bool(b)
            }
            e => todo!("deserialize any for {:?}", e),
        }
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let header = self.read_header()?;
        visitor.visit_bool(self.read_bool(header)?)
    }

    fn deserialize_i8<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let header = self.read_header()?;
        visitor.visit_i8(self.read_integer(header)?)
    }

    fn deserialize_i16<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let header = self.read_header()?;
        visitor.visit_i16(self.read_integer(header)?)
    }

    fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let header = self.read_header()?;
        visitor.visit_i32(self.read_integer(header)?)
    }

    fn deserialize_i64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let header = self.read_header()?;
        visitor.visit_i64(self.read_integer(header)?)
    }

    fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let header = self.read_header()?;
        visitor.visit_u8(self.read_integer(header)?)
    }

    fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let header = self.read_header()?;
        visitor.visit_u16(self.read_integer(header)?)
    }

    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let header = self.read_header()?;
        visitor.visit_u32(self.read_integer(header)?)
    }

    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let header = self.read_header()?;
        visitor.visit_u64(self.read_integer(header)?)
    }

    fn deserialize_option<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_unit<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_unit_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_newtype_struct<V>(
        self,
        name: &'static str,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_seq<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_tuple<V>(
        self,
        len: usize,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_tuple_struct<V>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_map<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_identifier<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_ignored_any<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_f32<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_f64<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_char<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_str<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_string<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_bytes<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }

    fn deserialize_byte_buf<V>(
        self,
        visitor: V,
    ) -> std::prelude::v1::Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_header(bytes: &[u8], expected: Header) {
        let mut de = Deserializer::from_bytes(bytes);
        let header = de.read_header().unwrap();
        assert_eq!(header, expected);
    }

    #[test]
    fn test_read_header() {
        assert_header(
            &[0b_0000_0000],
            Header {
                element_type: ElementType::Null,
                payload_size: 0,
            },
        );
        assert_header(
            &[0b_0000_0001],
            Header {
                element_type: ElementType::True,
                payload_size: 0,
            },
        );
        assert_header(
            &[0b_0000_0010],
            Header {
                element_type: ElementType::False,
                payload_size: 0,
            },
        );
        assert_header(
            &[0b_1100_0011, 0xFA],
            Header {
                element_type: ElementType::Int,
                payload_size: 0xFA,
            },
        );
        assert_header(
            b"\xf3\x00\x00\x00\x00\x00\x00\x00\x01",
            Header {
                element_type: ElementType::Int,
                payload_size: 1,
            },
        );
    }

    fn assert_all_int_types_eq(encoded: &[u8], expected: i64) {
        // unsigned
        assert_eq!(
            from_bytes::<i8>(encoded).unwrap(),
            expected as i8,
            "parsing {encoded:?} as i8"
        );
        assert_eq!(from_bytes::<i16>(encoded).unwrap(), expected as i16);
        assert_eq!(from_bytes::<i32>(encoded).unwrap(), expected as i32);
        assert_eq!(from_bytes::<i64>(encoded).unwrap(), expected);
        // signed
        assert_eq!(from_bytes::<u8>(encoded).unwrap(), expected as u8);
        assert_eq!(from_bytes::<u16>(encoded).unwrap(), expected as u16);
        assert_eq!(from_bytes::<u32>(encoded).unwrap(), expected as u32);
        assert_eq!(from_bytes::<u64>(encoded).unwrap(), expected as u64);
    }

    #[test]
    fn test_decoding_1() {
        /* From the spec:
        The header for an element does not need to be in its simplest form. For example, consider the JSON numeric value "1". That element can be encode in five different ways:
           0x13 0x31
           0xc3 0x01 0x31
           0xd3 0x00 0x01 0x31
           0xe3 0x00 0x00 0x00 0x01 0x31
           0xf3 0x00 0x00 0x00 0x00 0x00 0x00 0x00 0x01 0x31
        */
        assert_all_int_types_eq(b"\x13\x31", 1);
        assert_all_int_types_eq(b"\xc3\x01\x31", 1);
        assert_all_int_types_eq(b"\xd3\x00\x01\x31", 1);
        assert_all_int_types_eq(b"\xe3\x00\x00\x00\x01\x31", 1);
        assert_all_int_types_eq(b"\xf3\x00\x00\x00\x00\x00\x00\x00\x01\x31", 1);
        assert_all_int_types_eq(b"\xc3\x03127", 127);
    }

    #[test]
    fn test_decoding_large_int() {
        assert_eq!(
            from_bytes::<u64>(b"\xc3\xf418446744073709551615").unwrap(),
            18446744073709551615
        );
        // large negative i64
        assert_eq!(
            from_bytes::<i64>(b"\xc3\xf5-9223372036854775808").unwrap(),
            -9223372036854775808
        );
    }
}
