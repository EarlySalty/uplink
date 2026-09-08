//! AMF0 decoder
// Modified by Uplink: bounded native/Serde decoding; see ../PATCHES.md.

use std::io;
use std::io::Read;

use num_traits::FromPrimitive;
use scuffle_bytes_util::StringCow;
use scuffle_bytes_util::zero_copy::ZeroCopyReader;

use crate::{Amf0Array, Amf0Error, Amf0Marker, Amf0Object, Amf0Value};

/// Resource budgets shared by native and Serde decoding for one input message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    /// Maximum encoded bytes, including markers, lengths, names and terminators.
    pub max_input_bytes: usize,
    /// Maximum encoded bytes in each string, object key or class name.
    pub max_string_bytes: usize,
    /// Maximum actual entries in each object or array (including duplicate keys).
    pub max_container_entries: usize,
    /// Maximum values, including containers, over the decoder's entire lifetime.
    pub max_total_values: usize,
    /// Maximum simultaneously nested containers; a root container has depth one.
    pub max_depth: usize,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024,
            max_string_bytes: 16 * 1024,
            max_container_entries: 128,
            max_total_values: 2048,
            max_depth: 16,
        }
    }
}

/// AMF0 decoder.
///
/// Provides various functions to decode different types of AMF0 values.
#[derive(Debug, Clone)]
pub struct Amf0Decoder<R> {
    pub(crate) reader: R,
    pub(crate) next_marker: Option<Amf0Marker>,
    limits: DecodeLimits,
    bytes_read: usize,
    values_read: usize,
    depth: usize,
    input_length: Option<usize>,
    failed_read: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObjectHeader<'a> {
    Object,
    TypedObject { name: StringCow<'a> },
    EcmaArray { size: u32 },
}

impl<B> Amf0Decoder<scuffle_bytes_util::zero_copy::BytesBuf<B>>
where
    B: bytes::Buf,
{
    /// Create a new deserializer from a buffer implementing [`bytes::Buf`].
    pub fn from_buf(buf: B) -> Self {
        Self::from_buf_with_limits(buf, DecodeLimits::default())
    }

    /// Create a buffer decoder with explicit budgets.
    pub fn from_buf_with_limits(buf: B, limits: DecodeLimits) -> Self {
        let length = buf.remaining();
        let mut decoder = Self::with_limits(buf.into(), limits);
        decoder.input_length = Some(length);
        decoder
    }
}

impl<R> Amf0Decoder<scuffle_bytes_util::zero_copy::IoRead<R>>
where
    R: std::io::Read,
{
    /// Create a new deserializer from a reader implementing [`std::io::Read`].
    pub fn from_reader(reader: R) -> Self {
        Self::from_reader_with_limits(reader, DecodeLimits::default())
    }

    /// Create an IO decoder with explicit budgets; stream length need not be known.
    pub fn from_reader_with_limits(reader: R, limits: DecodeLimits) -> Self {
        Self::with_limits(reader.into(), limits)
    }
}

impl<'a> Amf0Decoder<scuffle_bytes_util::zero_copy::Slice<'a>> {
    /// Create a new deserializer from a byte slice.
    pub fn from_slice(slice: &'a [u8]) -> Amf0Decoder<scuffle_bytes_util::zero_copy::Slice<'a>> {
        Self::from_slice_with_limits(slice, DecodeLimits::default())
    }

    /// Create a borrowed decoder with explicit budgets.
    pub fn from_slice_with_limits(slice: &'a [u8], limits: DecodeLimits) -> Self {
        let mut decoder = Self::with_limits(slice.into(), limits);
        decoder.input_length = Some(slice.len());
        decoder
    }
}

impl<R> Amf0Decoder<R> {
    /// Create a decoder over a zero-copy reader with explicit budgets.
    pub fn with_limits(reader: R, limits: DecodeLimits) -> Self {
        Self {
            reader,
            next_marker: None,
            limits,
            bytes_read: 0,
            values_read: 0,
            depth: 0,
            input_length: None,
            failed_read: false,
        }
    }

    fn check_bytes(&self, size: usize) -> Result<(), Amf0Error> {
        if self.failed_read {
            return Err(Amf0Error::DecoderFailed);
        }
        if self
            .input_length
            .is_some_and(|n| n > self.limits.max_input_bytes)
            || size > self.limits.max_input_bytes.saturating_sub(self.bytes_read)
        {
            return Err(Amf0Error::DecodeLimit("input bytes"));
        }
        if self
            .input_length
            .is_some_and(|n| size > n.saturating_sub(self.bytes_read))
        {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
        Ok(())
    }

    pub(crate) fn check_entries(&self, count: usize) -> Result<(), Amf0Error> {
        if count > self.limits.max_container_entries {
            return Err(Amf0Error::DecodeLimit("container entries"));
        }
        Ok(())
    }

    #[cfg(feature = "serde")]
    pub(crate) fn progress(&self) -> usize {
        self.values_read
    }

    #[cfg(feature = "serde")]
    pub(crate) fn require_progress(&self, before: usize) -> Result<(), Amf0Error> {
        if self.values_read == before {
            return Err(Amf0Error::IncompleteContainer);
        }
        Ok(())
    }

    pub(crate) fn with_container<T>(
        &mut self,
        decode: impl FnOnce(&mut Self) -> Result<T, Amf0Error>,
    ) -> Result<T, Amf0Error> {
        if self.depth >= self.limits.max_depth {
            return Err(Amf0Error::DecodeLimit("nesting depth"));
        }
        self.depth += 1;
        let result = decode(self);
        self.depth -= 1;
        result
    }
}

impl<'a, R> Amf0Decoder<R>
where
    R: ZeroCopyReader<'a>,
{
    fn read_fixed<const N: usize>(&mut self) -> Result<[u8; N], Amf0Error> {
        self.check_bytes(N)?;
        let mut bytes = [0; N];
        if let Err(error) = self.reader.as_std().read_exact(&mut bytes) {
            self.failed_read = true;
            return Err(error.into());
        }
        self.bytes_read += N;
        Ok(bytes)
    }

    fn read_string_bytes(&mut self, len: usize) -> Result<StringCow<'a>, Amf0Error> {
        if len > self.limits.max_string_bytes {
            return Err(Amf0Error::DecodeLimit("string bytes"));
        }
        self.check_bytes(len)?;
        let bytes = match self.reader.try_read(len) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.failed_read = true;
                return Err(error.into());
            }
        };
        if bytes.as_bytes().len() != len {
            self.failed_read = true;
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
        self.bytes_read += len;
        Ok(StringCow::from_bytes(bytes.into_bytes().try_into()?))
    }

    /// Require the entire message to have been consumed.
    ///
    /// Incremental decoding remains available through the decoder methods.
    pub fn finish(&mut self) -> Result<(), Amf0Error> {
        if self.has_remaining()? {
            return Err(Amf0Error::TrailingData);
        }
        Ok(())
    }

    /// Decode a [`Amf0Value`] from the buffer.
    pub fn decode_value(&mut self) -> Result<Amf0Value<'a>, Amf0Error> {
        let marker = self.peek_marker()?;

        match marker {
            Amf0Marker::Boolean => self.decode_boolean().map(Into::into),
            Amf0Marker::Number | Amf0Marker::Date => self.decode_number().map(Into::into),
            Amf0Marker::String | Amf0Marker::LongString | Amf0Marker::XmlDocument => {
                self.decode_string().map(Into::into)
            }
            Amf0Marker::Null | Amf0Marker::Undefined => self.decode_null().map(|_| Amf0Value::Null),
            Amf0Marker::Object | Amf0Marker::TypedObject | Amf0Marker::EcmaArray => {
                self.decode_object().map(Into::into)
            }
            Amf0Marker::StrictArray => self.decode_strict_array().map(Into::into),
            _ => Err(Amf0Error::UnsupportedMarker(marker)),
        }
    }

    /// Decode all values from the buffer until the end.
    pub fn decode_all(&mut self) -> Result<Vec<Amf0Value<'a>>, Amf0Error> {
        let mut values = Vec::new();

        while self.has_remaining()? {
            values.push(self.decode_value()?);
        }

        Ok(values)
    }

    /// Convert the decoder into an iterator over the values in the buffer.
    pub fn stream(&mut self) -> Amf0DecoderStream<'_, 'a, R> {
        Amf0DecoderStream {
            decoder: self,
            _marker: std::marker::PhantomData,
        }
    }

    /// Check if there are any values left in the buffer.
    pub fn has_remaining(&mut self) -> Result<bool, Amf0Error> {
        self.check_bytes(0)?;
        if self.next_marker.is_none() && self.input_length == Some(self.bytes_read) {
            return Ok(false);
        }
        // A finite IO stream can end exactly at the byte budget. Probe one byte
        // without allocation so EOF stays distinct from an oversized message.
        if self.next_marker.is_none() && self.bytes_read == self.limits.max_input_bytes {
            let mut probe = [0];
            return match self.reader.as_std().read(&mut probe) {
                Ok(0) => Ok(false),
                Ok(_) => {
                    self.failed_read = true;
                    Err(Amf0Error::DecodeLimit("input bytes"))
                }
                Err(error) => {
                    self.failed_read = true;
                    Err(error.into())
                }
            };
        }
        match self.peek_marker() {
            Ok(_) => Ok(true),
            Err(Amf0Error::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                self.failed_read = false;
                Ok(false)
            }
            Err(err) => Err(err),
        }
    }

    /// Peek the next marker in the buffer without consuming it.
    pub fn peek_marker(&mut self) -> Result<Amf0Marker, Amf0Error> {
        let marker = self.read_marker()?;
        // Buffer the marker for the next read
        self.next_marker = Some(marker);

        Ok(marker)
    }

    fn read_marker(&mut self) -> Result<Amf0Marker, Amf0Error> {
        if let Some(marker) = self.next_marker.take() {
            return Ok(marker);
        }

        let marker = self.read_fixed::<1>()?[0];
        let marker = Amf0Marker::from_u8(marker).ok_or(Amf0Error::UnknownMarker(marker))?;
        Ok(marker)
    }

    fn expect_marker(&mut self, expect: &'static [Amf0Marker]) -> Result<Amf0Marker, Amf0Error> {
        let marker = self.read_marker()?;

        if !expect.contains(&marker) {
            Err(Amf0Error::UnexpectedType {
                expected: expect,
                got: marker,
            })
        } else {
            if self.values_read >= self.limits.max_total_values {
                return Err(Amf0Error::DecodeLimit("total values"));
            }
            self.values_read += 1;
            Ok(marker)
        }
    }

    /// Decode a number from the buffer.
    pub fn decode_number(&mut self) -> Result<f64, Amf0Error> {
        let marker = self.expect_marker(&[Amf0Marker::Number, Amf0Marker::Date])?;

        let number = f64::from_be_bytes(self.read_fixed()?);

        if marker == Amf0Marker::Date {
            // Skip the timezone
            self.read_fixed::<2>()?;
        }

        Ok(number)
    }

    /// Decode a boolean from the buffer.
    pub fn decode_boolean(&mut self) -> Result<bool, Amf0Error> {
        self.expect_marker(&[Amf0Marker::Boolean])?;
        let value = self.read_fixed::<1>()?[0];
        Ok(value != 0)
    }

    pub(crate) fn decode_normal_string(&mut self) -> Result<StringCow<'a>, Amf0Error> {
        let len = u16::from_be_bytes(self.read_fixed()?) as usize;
        self.read_string_bytes(len)
    }

    /// Decode a string from the buffer.
    ///
    /// This function can decode both normal strings and long strings.
    pub fn decode_string(&mut self) -> Result<StringCow<'a>, Amf0Error> {
        let marker = self.expect_marker(&[
            Amf0Marker::String,
            Amf0Marker::LongString,
            Amf0Marker::XmlDocument,
        ])?;

        let len = if marker == Amf0Marker::String {
            u16::from_be_bytes(self.read_fixed()?) as usize
        } else {
            // LongString or XmlDocument
            u32::from_be_bytes(self.read_fixed()?) as usize
        };

        self.read_string_bytes(len)
    }

    /// Decode a null value from the buffer.
    ///
    /// This function can also decode undefined values.
    pub fn decode_null(&mut self) -> Result<(), Amf0Error> {
        self.expect_marker(&[Amf0Marker::Null, Amf0Marker::Undefined])?;
        Ok(())
    }

    /// Deserialize a value from the buffer using [serde].
    #[cfg(feature = "serde")]
    pub fn deserialize<T>(&mut self) -> Result<T, Amf0Error>
    where
        T: serde::de::Deserialize<'a>,
    {
        T::deserialize(self)
    }

    /// Deserialize a stream of values from the buffer using [serde].
    #[cfg(feature = "serde")]
    pub fn deserialize_stream<T>(&mut self) -> crate::de::Amf0DeserializerStream<'_, R, T>
    where
        T: serde::de::Deserialize<'a>,
    {
        crate::de::Amf0DeserializerStream::new(self)
    }

    // --- Object and Ecma array ---

    pub(crate) fn decode_object_header(&mut self) -> Result<ObjectHeader<'a>, Amf0Error> {
        let marker = self.expect_marker(&[
            Amf0Marker::Object,
            Amf0Marker::TypedObject,
            Amf0Marker::EcmaArray,
        ])?;

        if marker == Amf0Marker::Object {
            Ok(ObjectHeader::Object)
        } else if marker == Amf0Marker::TypedObject {
            let name = self.decode_normal_string()?;
            Ok(ObjectHeader::TypedObject { name })
        } else {
            // EcmaArray
            let size = u32::from_be_bytes(self.read_fixed()?);
            Ok(ObjectHeader::EcmaArray { size })
        }
    }

    pub(crate) fn decode_object_key(&mut self) -> Result<Option<StringCow<'a>>, Amf0Error> {
        // Object keys are not preceeded with a marker and are always normal strings
        let key = self.decode_normal_string()?;

        // The object end marker is preceeded by an empty string
        if key.as_str().is_empty() {
            // Check if the next marker is an object end marker
            if self.peek_marker()? == Amf0Marker::ObjectEnd {
                // Clear the next marker buffer
                self.next_marker = None;

                return Ok(None);
            }
        }

        Ok(Some(key))
    }

    /// Decode an object from the buffer.
    ///
    /// This function can decode normal objects, typed objects and ECMA arrays.
    pub fn decode_object(&mut self) -> Result<Amf0Object<'a>, Amf0Error> {
        self.with_container(|decoder| {
            // ECMA count is a hint, never an allocation size or termination rule.
            decoder.decode_object_header()?;
            let mut object = Amf0Object::new();
            let mut count = 0usize;
            while let Some(key) = decoder.decode_object_key()? {
                count = count
                    .checked_add(1)
                    .ok_or(Amf0Error::DecodeLimit("container entries"))?;
                decoder.check_entries(count)?;
                let value = decoder.decode_value()?;
                object.insert(key, value);
            }
            Ok(object)
        })
    }

    // --- Strict array ---

    pub(crate) fn decode_strict_array_header(&mut self) -> Result<u32, Amf0Error> {
        self.expect_marker(&[Amf0Marker::StrictArray])?;
        let size = u32::from_be_bytes(self.read_fixed()?);
        self.check_entries(size as usize)?;
        if size as usize
            > self
                .limits
                .max_total_values
                .saturating_sub(self.values_read)
        {
            return Err(Amf0Error::DecodeLimit("total values"));
        }
        // Each child needs at least one marker byte. Check before exposing a
        // Serde size_hint or making an allocation.
        self.check_bytes(size as usize)?;
        Ok(size)
    }

    /// Decode a strict array from the buffer.
    pub fn decode_strict_array(&mut self) -> Result<Amf0Array<'a>, Amf0Error> {
        self.with_container(|decoder| {
            let size = decoder.decode_strict_array_header()?;
            let mut array = Vec::new();
            for _ in 0..size {
                array.push(decoder.decode_value()?);
            }
            Ok(Amf0Array::from(array))
        })
    }
}

/// An iterator over the values in the buffer.
///
/// Yields values of type [`Amf0Value`] until the end of the buffer is reached.
#[must_use = "Iterators are lazy and do nothing unless consumed"]
pub struct Amf0DecoderStream<'a, 'de, R> {
    decoder: &'a mut Amf0Decoder<R>,
    _marker: std::marker::PhantomData<&'de ()>,
}

impl<'de, R: ZeroCopyReader<'de>> Iterator for Amf0DecoderStream<'_, 'de, R> {
    type Item = Result<Amf0Value<'de>, Amf0Error>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.decoder.has_remaining() {
            Ok(true) => Some(self.decoder.decode_value()),
            Ok(false) => None,
            Err(err) => Some(Err(err)),
        }
    }
}

impl<'de, R> std::iter::FusedIterator for Amf0DecoderStream<'_, 'de, R> where R: ZeroCopyReader<'de> {}

#[cfg(test)]
#[cfg_attr(all(test, coverage_nightly), coverage(off))]
mod tests {
    use super::Amf0Decoder;
    use crate::{Amf0Marker, Amf0Value};

    #[test]
    fn strict_array() {
        #[rustfmt::skip]
        let bytes = [
            Amf0Marker::StrictArray as u8,
            0, 0, 0, 2, // size
            Amf0Marker::String as u8,
            0, 3, b'v', b'a', b'l', // value
            Amf0Marker::Boolean as u8,
            1, // value
        ];

        let mut decoder = Amf0Decoder::from_slice(&bytes);
        let array = decoder.decode_strict_array().unwrap();
        assert_eq!(array.len(), 2);
        assert_eq!(array[0], Amf0Value::String("val".into()));
        assert_eq!(array[1], Amf0Value::Boolean(true));
    }

    #[test]
    fn ecma_array() {
        #[rustfmt::skip]
        let bytes = [
            Amf0Marker::EcmaArray as u8,
            0, 0, 0, 2, // size
            0, 3, b'a', b'b', b'c', // key
            Amf0Marker::String as u8,
            0, 3, b'v', b'a', b'l', // value
            0, 4, b'd', b'e', b'f', b'g', // key
            Amf0Marker::Boolean as u8,
            1, // value
            0, 0, Amf0Marker::ObjectEnd as u8,
        ];

        let mut decoder = Amf0Decoder::from_slice(&bytes);
        let object = decoder.decode_object().unwrap();
        assert_eq!(object.len(), 2);
        assert_eq!(
            *object.get(&"abc".into()).unwrap(),
            Amf0Value::String("val".into())
        );
        assert_eq!(
            *object.get(&"defg".into()).unwrap(),
            Amf0Value::Boolean(true)
        );
    }

    #[test]
    fn decoder_stream() {
        #[rustfmt::skip]
        let bytes = [
            Amf0Marker::Boolean as u8,
            1, // value
            Amf0Marker::String as u8,
            0, 3, b'a', b'b', b'c', // value
            Amf0Marker::Null as u8,
        ];

        let mut decoder = Amf0Decoder::from_slice(&bytes);
        let mut stream = decoder.stream();
        assert_eq!(stream.next().unwrap().unwrap(), Amf0Value::Boolean(true));
        assert_eq!(
            stream.next().unwrap().unwrap(),
            Amf0Value::String("abc".into())
        );
        assert_eq!(stream.next().unwrap().unwrap(), Amf0Value::Null);
        assert!(stream.next().is_none());
    }
}
