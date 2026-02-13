use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::varint::VarInt;

// メッセージタイプ ID
const MSG_CLIENT_SETUP: u64 = 0x40;
const MSG_SERVER_SETUP: u64 = 0x41;
const MSG_SUBSCRIBE: u64 = 0x03;
const MSG_SUBSCRIBE_OK: u64 = 0x04;
const MSG_PUBLISH: u64 = 0x06;
const MSG_PUBLISH_OK: u64 = 0x07;

const MOQ_VERSION_DRAFT: u64 = 0xff000001; // draft version

#[derive(Debug, Clone)]
pub enum Message {
    ClientSetup(ClientSetup),
    ServerSetup(ServerSetup),
    Subscribe(Subscribe),
    SubscribeOk(SubscribeOk),
    Publish(Publish),
    PublishOk(PublishOk),
}

#[derive(Debug, Clone)]
pub struct ClientSetup {
    pub supported_versions: Vec<u64>,
}

impl Default for ClientSetup {
    fn default() -> Self {
        Self {
            supported_versions: vec![MOQ_VERSION_DRAFT],
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerSetup {
    pub selected_version: u64,
}

impl Default for ServerSetup {
    fn default() -> Self {
        Self {
            selected_version: MOQ_VERSION_DRAFT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Subscribe {
    pub track_namespace: String,
    pub track_name: String,
}

#[derive(Debug, Clone)]
pub struct SubscribeOk {
    pub track_namespace: String,
    pub track_name: String,
}

#[derive(Debug, Clone)]
pub struct Publish {
    pub track_namespace: String,
    pub track_name: String,
}

#[derive(Debug, Clone)]
pub struct PublishOk {
    pub track_namespace: String,
    pub track_name: String,
}

/// Object ヘッダ (unidirectional stream で送られる)
#[derive(Debug, Clone)]
pub struct ObjectHeader {
    pub track_namespace: String,
    pub track_name: String,
    pub group_id: u64,
    pub object_id: u64,
    pub payload_length: u64,
}

// --- エンコード ---

fn encode_string<B: BufMut>(buf: &mut B, s: &str) {
    VarInt::from_u64(s.len() as u64).unwrap().encode(buf);
    buf.put_slice(s.as_bytes());
}

fn decode_string<B: Buf>(buf: &mut B) -> anyhow::Result<String> {
    let len = VarInt::decode(buf)?.into_inner() as usize;
    if buf.remaining() < len {
        anyhow::bail!("not enough data for string (need {len} bytes)");
    }
    let mut data = vec![0u8; len];
    buf.copy_to_slice(&mut data);
    String::from_utf8(data).map_err(|e| anyhow::anyhow!("invalid utf-8: {e}"))
}

impl Message {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::new();
        match self {
            Message::ClientSetup(m) => {
                VarInt::from_u64(MSG_CLIENT_SETUP).unwrap().encode(&mut buf);
                VarInt::from_u64(m.supported_versions.len() as u64)
                    .unwrap()
                    .encode(&mut buf);
                for v in &m.supported_versions {
                    VarInt::from_u64(*v).unwrap().encode(&mut buf);
                }
                // setup parameters (num=0)
                VarInt::from_u64(0).unwrap().encode(&mut buf);
            }
            Message::ServerSetup(m) => {
                VarInt::from_u64(MSG_SERVER_SETUP).unwrap().encode(&mut buf);
                VarInt::from_u64(m.selected_version).unwrap().encode(&mut buf);
                // setup parameters (num=0)
                VarInt::from_u64(0).unwrap().encode(&mut buf);
            }
            Message::Subscribe(m) => {
                VarInt::from_u64(MSG_SUBSCRIBE).unwrap().encode(&mut buf);
                encode_string(&mut buf, &m.track_namespace);
                encode_string(&mut buf, &m.track_name);
            }
            Message::SubscribeOk(m) => {
                VarInt::from_u64(MSG_SUBSCRIBE_OK).unwrap().encode(&mut buf);
                encode_string(&mut buf, &m.track_namespace);
                encode_string(&mut buf, &m.track_name);
            }
            Message::Publish(m) => {
                VarInt::from_u64(MSG_PUBLISH).unwrap().encode(&mut buf);
                encode_string(&mut buf, &m.track_namespace);
                encode_string(&mut buf, &m.track_name);
            }
            Message::PublishOk(m) => {
                VarInt::from_u64(MSG_PUBLISH_OK).unwrap().encode(&mut buf);
                encode_string(&mut buf, &m.track_namespace);
                encode_string(&mut buf, &m.track_name);
            }
        }
        buf.freeze()
    }

    pub fn decode(buf: &mut Bytes) -> anyhow::Result<Self> {
        let msg_type = VarInt::decode(buf)?.into_inner();
        match msg_type {
            MSG_CLIENT_SETUP => {
                let num_versions = VarInt::decode(buf)?.into_inner() as usize;
                let mut supported_versions = Vec::with_capacity(num_versions);
                for _ in 0..num_versions {
                    supported_versions.push(VarInt::decode(buf)?.into_inner());
                }
                // skip setup parameters
                let num_params = VarInt::decode(buf)?.into_inner() as usize;
                for _ in 0..num_params {
                    let _key = VarInt::decode(buf)?;
                    let _val = decode_string(buf)?;
                }
                Ok(Message::ClientSetup(ClientSetup { supported_versions }))
            }
            MSG_SERVER_SETUP => {
                let selected_version = VarInt::decode(buf)?.into_inner();
                let num_params = VarInt::decode(buf)?.into_inner() as usize;
                for _ in 0..num_params {
                    let _key = VarInt::decode(buf)?;
                    let _val = decode_string(buf)?;
                }
                Ok(Message::ServerSetup(ServerSetup { selected_version }))
            }
            MSG_SUBSCRIBE => {
                let track_namespace = decode_string(buf)?;
                let track_name = decode_string(buf)?;
                Ok(Message::Subscribe(Subscribe {
                    track_namespace,
                    track_name,
                }))
            }
            MSG_SUBSCRIBE_OK => {
                let track_namespace = decode_string(buf)?;
                let track_name = decode_string(buf)?;
                Ok(Message::SubscribeOk(SubscribeOk {
                    track_namespace,
                    track_name,
                }))
            }
            MSG_PUBLISH => {
                let track_namespace = decode_string(buf)?;
                let track_name = decode_string(buf)?;
                Ok(Message::Publish(Publish {
                    track_namespace,
                    track_name,
                }))
            }
            MSG_PUBLISH_OK => {
                let track_namespace = decode_string(buf)?;
                let track_name = decode_string(buf)?;
                Ok(Message::PublishOk(PublishOk {
                    track_namespace,
                    track_name,
                }))
            }
            other => anyhow::bail!("unknown message type: 0x{other:x}"),
        }
    }
}

impl ObjectHeader {
    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        encode_string(buf, &self.track_namespace);
        encode_string(buf, &self.track_name);
        VarInt::from_u64(self.group_id).unwrap().encode(buf);
        VarInt::from_u64(self.object_id).unwrap().encode(buf);
        VarInt::from_u64(self.payload_length).unwrap().encode(buf);
    }

    pub fn decode<B: Buf>(buf: &mut B) -> anyhow::Result<Self> {
        let track_namespace = decode_string(buf)?;
        let track_name = decode_string(buf)?;
        let group_id = VarInt::decode(buf)?.into_inner();
        let object_id = VarInt::decode(buf)?.into_inner();
        let payload_length = VarInt::decode(buf)?.into_inner();
        Ok(Self {
            track_namespace,
            track_name,
            group_id,
            object_id,
            payload_length,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_client_setup() {
        let msg = Message::ClientSetup(ClientSetup::default());
        let encoded = msg.encode();
        let decoded = Message::decode(&mut encoded.clone()).unwrap();
        match decoded {
            Message::ClientSetup(cs) => {
                assert_eq!(cs.supported_versions, vec![MOQ_VERSION_DRAFT]);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn roundtrip_subscribe() {
        let msg = Message::Subscribe(Subscribe {
            track_namespace: "ns".into(),
            track_name: "track".into(),
        });
        let encoded = msg.encode();
        let decoded = Message::decode(&mut encoded.clone()).unwrap();
        match decoded {
            Message::Subscribe(s) => {
                assert_eq!(s.track_namespace, "ns");
                assert_eq!(s.track_name, "track");
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn roundtrip_object_header() {
        let hdr = ObjectHeader {
            track_namespace: "ns".into(),
            track_name: "track".into(),
            group_id: 1,
            object_id: 42,
            payload_length: 100,
        };
        let mut buf = BytesMut::new();
        hdr.encode(&mut buf);
        let decoded = ObjectHeader::decode(&mut buf.freeze()).unwrap();
        assert_eq!(decoded.track_namespace, "ns");
        assert_eq!(decoded.track_name, "track");
        assert_eq!(decoded.group_id, 1);
        assert_eq!(decoded.object_id, 42);
        assert_eq!(decoded.payload_length, 100);
    }
}
