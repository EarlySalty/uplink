// Local Uplink fork, modified 2026-09-08; see PATCHES.md for provenance and scope.
//! Reading [`NetConnectionCommand`].

use bytes::Bytes;
use scuffle_amf0::decoder::Amf0Decoder;
use scuffle_bytes_util::zero_copy::BytesBuf;

use super::NetConnectionCommand;
use crate::command_messages::error::CommandError;

impl NetConnectionCommand<'_> {
    /// Reads a [`NetConnectionCommand`] from the given decoder.
    ///
    /// Returns `Ok(None)` if the `command_name` is not recognized.
    pub fn read(
        command_name: &str,
        decoder: &mut Amf0Decoder<BytesBuf<Bytes>>,
    ) -> Result<Option<Self>, CommandError> {
        match command_name {
            "connect" => {
                let command_object = decoder.deserialize()?;
                Ok(Some(Self::Connect(command_object)))
            }
            "call" => Ok(Some(Self::Call {
                command_object: decoder.deserialize()?,
                optional_arguments: decoder.deserialize()?,
            })),
            "close" => Ok(Some(Self::Close)),
            "createStream" => Ok(Some(Self::CreateStream)),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
#[cfg_attr(all(test, coverage_nightly), coverage(off))]
mod tests {
    use bytes::Bytes;
    use scuffle_amf0::Amf0Object;
    use scuffle_amf0::decoder::Amf0Decoder;
    use scuffle_amf0::encoder::Amf0Encoder;

    use super::NetConnectionCommand;
    use crate::command_messages::error::CommandError;

    #[test]
    fn test_read_no_app() {
        let mut command_object = Vec::new();
        let mut encoder = Amf0Encoder::new(&mut command_object);
        encoder.encode_object(&Amf0Object::new()).unwrap();

        let mut decoder = Amf0Decoder::from_buf(Bytes::from_owner(command_object));
        let result = NetConnectionCommand::read("connect", &mut decoder).unwrap_err();

        assert!(matches!(
            result,
            CommandError::Amf0(scuffle_amf0::Amf0Error::Custom(_))
        ));
    }

    #[test]
    fn connect_reads_enhanced_wire_capabilities() {
        use crate::command_messages::netconnection::CapsExMask;
        use scuffle_amf0::Amf0Value;
        let mut object = Amf0Object::new();
        object.insert("app".into(), Amf0Value::String("live".into()));
        object.insert("capsEx".into(), Amf0Value::Number(3.0));
        let mut bytes = Vec::new();
        Amf0Encoder::new(&mut bytes).encode_object(&object).unwrap();
        let mut decoder = Amf0Decoder::from_buf(Bytes::from_owner(bytes));
        let Some(NetConnectionCommand::Connect(connect)) =
            NetConnectionCommand::read("connect", &mut decoder).unwrap()
        else {
            panic!("connect fehlt")
        };
        let caps = connect
            .caps_ex
            .expect("capsEx muss im typisierten Feld landen");
        assert!(caps.contains(CapsExMask::Reconnect));
        assert!(caps.contains(CapsExMask::Multitrack));
        assert!(!connect.others.contains_key(&"capsEx".into()));
    }
}
