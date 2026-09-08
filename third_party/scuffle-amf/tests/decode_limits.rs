use std::collections::HashMap;
use std::fmt;

use scuffle_amf0::{Amf0Decoder, Amf0Error, Amf0Value, DecodeLimits};
use serde::de::{Deserialize, Deserializer, SeqAccess, Visitor};

fn array(count: u32) -> Vec<u8> {
    let mut bytes = vec![0x0a];
    bytes.extend_from_slice(&count.to_be_bytes());
    bytes.extend(std::iter::repeat_n(0x05, count as usize));
    bytes
}

#[test]
fn decoder_rejects_large_container() {
    assert!(Amf0Decoder::from_slice(&array(129)).decode_value().is_err());
}

#[test]
fn serde_rejects_large_container() {
    assert!(scuffle_amf0::from_slice::<Vec<Option<bool>>>(&array(129)).is_err());
}

#[test]
fn decoder_rejects_deep_nesting() {
    let bytes = [[0x0a, 0, 0, 0, 1].repeat(17), vec![0x05]].concat();
    assert!(Amf0Decoder::from_slice(&bytes).decode_value().is_err());
}

#[test]
fn serde_rejects_deep_nesting() {
    let bytes = [[0x0a, 0, 0, 0, 1].repeat(17), vec![0x05]].concat();
    assert!(scuffle_amf0::from_slice::<Amf0Value<'_>>(&bytes).is_err());
}

#[test]
fn strings_and_total_values_are_bounded() {
    let mut bytes = vec![0x0c, 0, 0, 0x40, 1];
    bytes.extend(std::iter::repeat_n(b'a', 16_385));
    assert!(Amf0Decoder::from_slice(&bytes).decode_value().is_err());
    assert!(scuffle_amf0::from_slice::<String>(&bytes).is_err());
    assert!(Amf0Decoder::from_slice(&[0x05; 2049]).decode_all().is_err());
}

#[test]
fn ecma_count_is_only_a_hint_and_end_is_consumed() {
    let bytes = [0x08, 0, 0, 0, 0, 0, 1, b'a', 0x05, 0, 0, 9, 0x01, 1];
    let mut decoder = Amf0Decoder::from_slice(&bytes);
    assert_eq!(decoder.decode_object().unwrap().len(), 1);
    assert!(decoder.decode_boolean().unwrap());
    let mut decoder = Amf0Decoder::from_slice(&bytes);
    assert_eq!(
        decoder
            .deserialize::<HashMap<String, Option<bool>>>()
            .unwrap()
            .len(),
        1
    );
    assert!(decoder.deserialize::<bool>().unwrap());
}

#[derive(Debug)]
struct SizeHint;

impl<'de> Deserialize<'de> for SizeHint {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Check;
        impl<'de> Visitor<'de> for Check {
            type Value = SizeHint;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded sequence")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                assert!(seq.size_hint().unwrap_or(0) <= 128, "untrusted size_hint");
                Ok(SizeHint)
            }
        }
        de.deserialize_seq(Check)
    }
}

#[test]
fn serde_size_hint_is_checked_before_visitor() {
    assert!(scuffle_amf0::from_slice::<SizeHint>(&[0x0a, 255, 255, 255, 255]).is_err());
}

#[test]
fn serde_rejects_unconsumed_container_and_trailing_data() {
    assert!(scuffle_amf0::from_slice::<SizeHint>(&array(1)).is_err());
    assert!(scuffle_amf0::from_slice::<bool>(&[0x01, 1, 0x05]).is_err());
}

#[test]
fn ecma_requires_actual_end_marker() {
    let bytes = [0x08, 0, 0, 0, 1, 0, 1, b'a', 0x05, 0, 0, 0];
    assert!(Amf0Decoder::from_slice(&bytes).decode_object().is_err());
    assert!(scuffle_amf0::from_slice::<HashMap<String, Option<bool>>>(&bytes).is_err());
}

#[test]
fn maximal_array_headers_do_not_allocate_from_wire_counts() {
    for marker in [0x08, 0x0a] {
        let bytes = [marker, 255, 255, 255, 255];
        assert!(Amf0Decoder::from_slice(&bytes).decode_value().is_err());
        assert!(
            Amf0Decoder::from_reader(bytes.as_slice())
                .decode_value()
                .is_err()
        );
        assert!(scuffle_amf0::from_slice::<Amf0Value<'_>>(&bytes).is_err());
    }
    // An enormous ECMA hint remains legal when the actual object is bounded.
    let bytes = [0x08, 255, 255, 255, 255, 0, 0, 9];
    assert!(
        Amf0Decoder::from_slice(&bytes)
            .decode_object()
            .unwrap()
            .is_empty()
    );
    assert!(
        scuffle_amf0::from_slice::<HashMap<String, bool>>(&bytes)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn custom_byte_limits_apply_to_slice_buf_and_io_before_decode() {
    let limits = DecodeLimits {
        max_input_bytes: 1,
        ..DecodeLimits::default()
    };
    let bytes = [0x01, 1];
    assert!(matches!(
        Amf0Decoder::from_slice_with_limits(&bytes, limits).decode_boolean(),
        Err(Amf0Error::DecodeLimit(_))
    ));
    assert!(matches!(
        Amf0Decoder::from_buf_with_limits(bytes.as_slice(), limits).deserialize::<bool>(),
        Err(Amf0Error::DecodeLimit(_))
    ));
    assert!(matches!(
        Amf0Decoder::from_reader_with_limits(bytes.as_slice(), limits).deserialize::<bool>(),
        Err(Amf0Error::DecodeLimit(_))
    ));

    let mut decoder = Amf0Decoder::from_reader_with_limits([0x05].as_slice(), limits);
    decoder.decode_null().unwrap();
    decoder.finish().unwrap();
    let mut decoder = Amf0Decoder::from_reader_with_limits([0x05, 0x05].as_slice(), limits);
    decoder.decode_null().unwrap();
    assert!(decoder.finish().is_err());
}

#[test]
fn value_budgets_persist_across_native_serde_and_optional_values() {
    let bytes = [0x05, 0x05, 0x05];
    let limits = DecodeLimits {
        max_total_values: 2,
        ..DecodeLimits::default()
    };
    let mut decoder = Amf0Decoder::from_slice_with_limits(&bytes, limits);
    decoder.decode_null().unwrap();
    assert_eq!(decoder.deserialize::<Option<bool>>().unwrap(), None);
    assert!(matches!(
        decoder.deserialize::<Option<bool>>(),
        Err(Amf0Error::DecodeLimit(_))
    ));
    let mut decoder = Amf0Decoder::from_slice_with_limits(&bytes, limits);
    assert!(
        decoder
            .deserialize::<scuffle_amf0::de::MultiValue<Vec<Option<bool>>>>()
            .is_err()
    );
}

#[test]
fn string_key_and_class_limits_apply_on_all_reader_kinds() {
    let limits = DecodeLimits {
        max_string_bytes: 1,
        ..DecodeLimits::default()
    };
    for bytes in [
        &[0x02, 0, 2, b'a', b'b'][..],
        &[0x0c, 0, 0, 0, 2, b'a', b'b'][..],
        &[0x0f, 0, 0, 0, 2, b'a', b'b'][..],
        &[0x03, 0, 2, b'a', b'b', 0x05, 0, 0, 9][..],
        &[0x10, 0, 2, b'a', b'b', 0, 0, 9][..],
    ] {
        assert!(matches!(
            Amf0Decoder::from_slice_with_limits(bytes, limits).decode_value(),
            Err(Amf0Error::DecodeLimit(_))
        ));
        assert!(matches!(
            Amf0Decoder::from_reader_with_limits(bytes, limits).deserialize::<Amf0Value<'_>>(),
            Err(Amf0Error::DecodeLimit(_))
        ));
    }
    for bytes in [&[0x02, 0, 1][..], &[0x0c, 255, 255, 255, 255][..]] {
        assert!(Amf0Decoder::from_reader(bytes).decode_value().is_err());
        assert!(scuffle_amf0::from_slice::<String>(bytes).is_err());
    }
}

fn object(entries: usize, ecma: bool) -> Vec<u8> {
    let mut bytes = if ecma {
        vec![0x08, 0, 0, 0, 0]
    } else {
        vec![0x03]
    };
    for _ in 0..entries {
        bytes.extend_from_slice(&[0, 1, b'a', 0x05]);
    }
    bytes.extend_from_slice(&[0, 0, 9]);
    bytes
}

#[test]
fn actual_object_entries_count_duplicates_and_ignore_ecma_hint() {
    let limits = DecodeLimits {
        max_container_entries: 1,
        ..DecodeLimits::default()
    };
    for ecma in [false, true] {
        assert!(
            Amf0Decoder::from_slice_with_limits(&object(1, ecma), limits)
                .decode_value()
                .is_ok()
        );
        assert!(
            Amf0Decoder::from_slice_with_limits(&object(2, ecma), limits)
                .decode_value()
                .is_err()
        );
        assert!(
            Amf0Decoder::from_slice_with_limits(&object(2, ecma), limits)
                .deserialize::<Amf0Value<'_>>()
                .is_err()
        );
    }
}

#[test]
fn depth_limits_are_exact_and_apply_to_ignored_values() {
    let limits = DecodeLimits {
        max_depth: 1,
        ..DecodeLimits::default()
    };
    assert!(
        Amf0Decoder::from_slice_with_limits(&array(1), limits)
            .decode_value()
            .is_ok()
    );
    let nested = [vec![0x0a, 0, 0, 0, 1], array(0)].concat();
    assert!(
        Amf0Decoder::from_slice_with_limits(&nested, limits)
            .decode_value()
            .is_err()
    );
    assert!(
        Amf0Decoder::from_slice_with_limits(&nested, limits)
            .deserialize::<serde::de::IgnoredAny>()
            .is_err()
    );
    let nested = [vec![0x03, 0, 1, b'a'], object(0, false), vec![0, 0, 9]].concat();
    assert!(
        Amf0Decoder::from_slice_with_limits(&nested, limits)
            .deserialize::<Amf0Value<'_>>()
            .is_err()
    );
}

#[test]
fn every_truncated_prefix_of_valid_values_fails() {
    for bytes in [
        array(2),
        object(2, false),
        object(2, true),
        vec![0x0c, 0, 0, 0, 2, b'a', b'b'],
    ] {
        for end in 0..bytes.len() {
            assert!(
                Amf0Decoder::from_slice(&bytes[..end])
                    .decode_value()
                    .is_err(),
                "native truncated at {end}: {bytes:?}"
            );
            assert!(
                scuffle_amf0::from_slice::<Amf0Value<'_>>(&bytes[..end]).is_err(),
                "serde truncated at {end}: {bytes:?}"
            );
        }
        assert!(Amf0Decoder::from_slice(&bytes).decode_value().is_ok());
        assert!(scuffle_amf0::from_slice::<Amf0Value<'_>>(&bytes).is_ok());
    }
}

#[test]
fn strict_lengths_are_checked_against_remaining_bytes() {
    let bytes = [0x0a, 0, 0, 0, 100];
    // Visitor must not receive even an in-budget hint when bytes cannot contain it.
    assert!(matches!(
        scuffle_amf0::from_slice::<SizeHint>(&bytes),
        Err(Amf0Error::Io(_))
    ));
}

#[test]
fn declared_string_size_is_checked_before_zero_copy_allocation() {
    struct NoAllocation(std::io::Cursor<&'static [u8]>);
    impl<'de> scuffle_bytes_util::zero_copy::ZeroCopyReader<'de> for NoAllocation {
        fn try_read(&mut self, _size: usize) -> std::io::Result<scuffle_bytes_util::BytesCow<'de>> {
            panic!("must reject declared size before calling allocating reader")
        }
        fn as_std(&mut self) -> impl std::io::Read {
            &mut self.0
        }
    }
    let reader = NoAllocation(std::io::Cursor::new(&[0x0c, 255, 255, 255, 255]));
    assert!(matches!(
        Amf0Decoder::with_limits(reader, DecodeLimits::default()).decode_value(),
        Err(Amf0Error::DecodeLimit(_))
    ));
}

#[derive(Debug)]
struct NoRead;

impl<'de> Deserialize<'de> for NoRead {
    fn deserialize<D: Deserializer<'de>>(_de: D) -> Result<Self, D::Error> {
        Ok(NoRead)
    }
}

#[test]
fn serde_seeds_cannot_claim_values_without_reading_them() {
    let bytes = array(1);
    assert!(scuffle_amf0::from_slice::<Vec<NoRead>>(&bytes).is_err());
    assert!(scuffle_amf0::from_slice::<HashMap<String, NoRead>>(&object(1, false)).is_err());
    let mut decoder = Amf0Decoder::from_slice(&[0x05]);
    assert!(
        decoder
            .deserialize::<scuffle_amf0::de::MultiValue<Vec<NoRead>>>()
            .is_err()
    );
    let mut decoder = Amf0Decoder::from_slice(&[0x05]);
    assert!(
        decoder
            .deserialize_stream::<NoRead>()
            .next()
            .unwrap()
            .is_err()
    );
}

#[derive(Debug)]
struct NoMapRead;

impl<'de> Deserialize<'de> for NoMapRead {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Check;
        impl<'de> Visitor<'de> for Check {
            type Value = NoMapRead;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                assert_eq!(map.size_hint(), None, "ECMA hints are not trusted");
                Ok(NoMapRead)
            }
        }
        de.deserialize_map(Check)
    }
}

#[test]
fn serde_map_visitor_must_consume_actual_entries() {
    for ecma in [false, true] {
        assert!(scuffle_amf0::from_slice::<NoMapRead>(&object(1, ecma)).is_err());
        assert!(scuffle_amf0::from_slice::<NoMapRead>(&object(0, ecma)).is_ok());
    }
    assert!(scuffle_amf0::from_slice::<NoMapRead>(&[0x08, 255, 255, 255, 255, 0, 0, 9]).is_ok());
}

#[test]
fn partial_io_reads_cannot_be_reused_as_a_new_value() {
    struct Truncated {
        calls: usize,
    }
    impl std::io::Read for Truncated {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            match self.calls {
                1 => {
                    output[0] = 0x00;
                    Ok(1)
                }
                2 => {
                    output[0] = 0x05;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }
    let mut decoder = Amf0Decoder::from_reader(Truncated { calls: 0 });
    assert!(matches!(decoder.decode_number(), Err(Amf0Error::Io(_))));
    assert!(matches!(
        decoder.has_remaining(),
        Err(Amf0Error::DecoderFailed)
    ));
    assert!(matches!(
        decoder.decode_null(),
        Err(Amf0Error::DecoderFailed)
    ));
}
