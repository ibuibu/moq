use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::varint::VarInt;

// メッセージタイプ ID (draft-ietf-moq-transport-15)
const MSG_CLIENT_SETUP: u64 = 0x20;
const MSG_SERVER_SETUP: u64 = 0x21;
const MSG_SUBSCRIBE: u64 = 0x03;
const MSG_SUBSCRIBE_OK: u64 = 0x04;
const MSG_PUBLISH: u64 = 0x1D;
const MSG_PUBLISH_OK: u64 = 0x1E;

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
pub enum SetupParameter {
    Role(u64),           // key 0x0, varint value
    Path(String),        // key 0x1, length-prefixed
    MaxSubscribeId(u64), // key 0x2, varint value
    Unknown(u64),        // 未知のパラメータ (スキップ済み)
}

#[derive(Debug, Clone)]
pub struct ClientSetup {
    pub params: Vec<SetupParameter>,
}

impl Default for ClientSetup {
    fn default() -> Self {
        Self { params: vec![] }
    }
}

#[derive(Debug, Clone)]
pub struct ServerSetup {
    pub params: Vec<SetupParameter>,
}

impl Default for ServerSetup {
    fn default() -> Self {
        Self { params: vec![] }
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

fn encode_setup_params<B: BufMut>(buf: &mut B, params: &[SetupParameter]) {
    VarInt::from_u64(params.len() as u64).unwrap().encode(buf);
    for p in params {
        match p {
            SetupParameter::Role(val) => {
                VarInt::from_u64(0x0).unwrap().encode(buf); // key
                let mut vbuf = BytesMut::new();
                VarInt::from_u64(*val).unwrap().encode(&mut vbuf);
                VarInt::from_u64(vbuf.len() as u64).unwrap().encode(buf); // value length
                buf.put_slice(&vbuf);
            }
            SetupParameter::Path(s) => {
                VarInt::from_u64(0x1).unwrap().encode(buf); // key
                encode_string(buf, s); // length-prefixed string
            }
            SetupParameter::MaxSubscribeId(val) => {
                VarInt::from_u64(0x2).unwrap().encode(buf); // key
                let mut vbuf = BytesMut::new();
                VarInt::from_u64(*val).unwrap().encode(&mut vbuf);
                VarInt::from_u64(vbuf.len() as u64).unwrap().encode(buf); // value length
                buf.put_slice(&vbuf);
            }
            SetupParameter::Unknown(_) => {} // encode 時はスキップ
        }
    }
}

fn decode_setup_params<B: Buf>(buf: &mut B) -> anyhow::Result<Vec<SetupParameter>> {
    let num_params = VarInt::decode(buf)?.into_inner() as usize;
    let mut params = Vec::with_capacity(num_params);
    for _ in 0..num_params {
        let key = VarInt::decode(buf)?.into_inner();
        match key {
            0x0 => {
                // Role: length-prefixed varint value
                let val_len = VarInt::decode(buf)?.into_inner() as usize;
                if buf.remaining() < val_len {
                    anyhow::bail!("not enough data for Role parameter");
                }
                let mut val_buf = buf.copy_to_bytes(val_len);
                let val = VarInt::decode(&mut val_buf)?.into_inner();
                params.push(SetupParameter::Role(val));
            }
            0x1 => {
                // Path: length-prefixed string
                let s = decode_string(buf)?;
                params.push(SetupParameter::Path(s));
            }
            0x2 => {
                // MaxSubscribeId: length-prefixed varint value
                let val_len = VarInt::decode(buf)?.into_inner() as usize;
                if buf.remaining() < val_len {
                    anyhow::bail!("not enough data for MaxSubscribeId parameter");
                }
                let mut val_buf = buf.copy_to_bytes(val_len);
                let val = VarInt::decode(&mut val_buf)?.into_inner();
                params.push(SetupParameter::MaxSubscribeId(val));
            }
            other => {
                // Unknown: skip value (length-prefixed)
                let val_len = VarInt::decode(buf)?.into_inner() as usize;
                if buf.remaining() < val_len {
                    anyhow::bail!("not enough data for unknown parameter 0x{other:x}");
                }
                buf.advance(val_len);
                params.push(SetupParameter::Unknown(other));
            }
        }
    }
    Ok(params)
}

impl Message {
    pub fn encode(&self) -> Bytes {
        let mut buf = BytesMut::new();
        match self {
            Message::ClientSetup(m) => {
                VarInt::from_u64(MSG_CLIENT_SETUP).unwrap().encode(&mut buf);
                // Payload を一時バッファにエンコード
                let mut payload = BytesMut::new();
                encode_setup_params(&mut payload, &m.params);
                // Length (u16 big-endian) + Payload
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::ServerSetup(m) => {
                VarInt::from_u64(MSG_SERVER_SETUP).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                encode_setup_params(&mut payload, &m.params);
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::Subscribe(m) => {
                VarInt::from_u64(MSG_SUBSCRIBE).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                encode_string(&mut payload, &m.track_namespace);
                encode_string(&mut payload, &m.track_name);
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::SubscribeOk(m) => {
                VarInt::from_u64(MSG_SUBSCRIBE_OK).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                encode_string(&mut payload, &m.track_namespace);
                encode_string(&mut payload, &m.track_name);
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::Publish(m) => {
                VarInt::from_u64(MSG_PUBLISH).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                encode_string(&mut payload, &m.track_namespace);
                encode_string(&mut payload, &m.track_name);
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::PublishOk(m) => {
                VarInt::from_u64(MSG_PUBLISH_OK).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                encode_string(&mut payload, &m.track_namespace);
                encode_string(&mut payload, &m.track_name);
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
        }
        buf.freeze()
    }

    pub fn decode(buf: &mut Bytes) -> anyhow::Result<Self> {
        let msg_type = VarInt::decode(buf)?.into_inner();
        // Length framing: u16 big-endian
        if buf.remaining() < 2 {
            anyhow::bail!("not enough data for message length");
        }
        let length = buf.get_u16() as usize;
        if buf.remaining() < length {
            anyhow::bail!("not enough data for message payload (need {length} bytes)");
        }
        let mut payload = buf.split_to(length);

        match msg_type {
            MSG_CLIENT_SETUP => {
                let params = decode_setup_params(&mut payload)?;
                Ok(Message::ClientSetup(ClientSetup { params }))
            }
            MSG_SERVER_SETUP => {
                let params = decode_setup_params(&mut payload)?;
                Ok(Message::ServerSetup(ServerSetup { params }))
            }
            MSG_SUBSCRIBE => {
                let track_namespace = decode_string(&mut payload)?;
                let track_name = decode_string(&mut payload)?;
                Ok(Message::Subscribe(Subscribe {
                    track_namespace,
                    track_name,
                }))
            }
            MSG_SUBSCRIBE_OK => {
                let track_namespace = decode_string(&mut payload)?;
                let track_name = decode_string(&mut payload)?;
                Ok(Message::SubscribeOk(SubscribeOk {
                    track_namespace,
                    track_name,
                }))
            }
            MSG_PUBLISH => {
                let track_namespace = decode_string(&mut payload)?;
                let track_name = decode_string(&mut payload)?;
                Ok(Message::Publish(Publish {
                    track_namespace,
                    track_name,
                }))
            }
            MSG_PUBLISH_OK => {
                let track_namespace = decode_string(&mut payload)?;
                let track_name = decode_string(&mut payload)?;
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
                assert!(cs.params.is_empty());
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn roundtrip_client_setup_with_params() {
        let msg = Message::ClientSetup(ClientSetup {
            params: vec![
                SetupParameter::Role(0x03),
                SetupParameter::Path("/moq".into()),
            ],
        });
        let encoded = msg.encode();
        let decoded = Message::decode(&mut encoded.clone()).unwrap();
        match decoded {
            Message::ClientSetup(cs) => {
                assert_eq!(cs.params.len(), 2);
                match &cs.params[0] {
                    SetupParameter::Role(v) => assert_eq!(*v, 0x03),
                    _ => panic!("expected Role"),
                }
                match &cs.params[1] {
                    SetupParameter::Path(s) => assert_eq!(s, "/moq"),
                    _ => panic!("expected Path"),
                }
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
