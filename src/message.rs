use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::varint::VarInt;

// メッセージタイプ ID (draft-ietf-moq-transport-15)
const MSG_CLIENT_SETUP: u64 = 0x20;
const MSG_SERVER_SETUP: u64 = 0x21;
const MSG_SUBSCRIBE: u64 = 0x03;
const MSG_SUBSCRIBE_OK: u64 = 0x04;
const MSG_REQUEST_ERROR: u64 = 0x05;
const MSG_UNSUBSCRIBE: u64 = 0x0A;
const MSG_SUBSCRIBE_DONE: u64 = 0x0B;
const MSG_GOAWAY: u64 = 0x10;
const MSG_PUBLISH: u64 = 0x1D;
const MSG_PUBLISH_OK: u64 = 0x1E;

/// Track namespace は N-tuple of byte strings (draft-15)
pub type TrackNamespace = Vec<String>;

pub const GROUP_ORDER_ASCENDING: u8 = 0x01;
pub const FILTER_NEXT_GROUP: u64 = 0x01;

#[derive(Debug, Clone)]
pub enum Message {
    ClientSetup(ClientSetup),
    ServerSetup(ServerSetup),
    Subscribe(Subscribe),
    SubscribeOk(SubscribeOk),
    RequestError(RequestError),
    Unsubscribe(Unsubscribe),
    SubscribeDone(SubscribeDone),
    GoAway(GoAway),
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
    pub subscribe_id: u64,
    pub track_alias: u64,
    pub track_namespace: TrackNamespace,
    pub track_name: String,
    pub subscriber_priority: u8,
    pub group_order: u8,
    pub filter_type: u64,
}

#[derive(Debug, Clone)]
pub struct SubscribeOk {
    pub subscribe_id: u64,
    pub expires: u64,
    pub group_order: u8,
    pub content_exists: bool,
}

#[derive(Debug, Clone)]
pub struct Publish {
    pub track_namespace: TrackNamespace,
    pub track_name: String,
    pub subscriber_priority: u8,
    pub group_order: u8,
}

#[derive(Debug, Clone)]
pub struct PublishOk {
    pub track_namespace: TrackNamespace,
    pub track_name: String,
}

#[derive(Debug, Clone)]
pub struct GoAway {
    pub new_session_uri: String,
}

#[derive(Debug, Clone)]
pub struct Unsubscribe {
    pub subscribe_id: u64,
}

#[derive(Debug, Clone)]
pub struct RequestError {
    pub subscribe_id: u64,
    pub error_code: u64,
    pub reason_phrase: String,
}

#[derive(Debug, Clone)]
pub struct SubscribeDone {
    pub subscribe_id: u64,
    pub status_code: u64,
    pub reason_phrase: String,
    pub content_exists: bool,
}

/// Subgroup stream ヘッダ (unidirectional stream の先頭、draft-15)
#[derive(Debug, Clone)]
pub struct SubgroupHeader {
    pub subscribe_id: u64,
    pub track_alias: u64,
    pub group_id: u64,
    pub subgroup_id: u64,
    pub publisher_priority: u8,
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

fn encode_tuple<B: BufMut>(buf: &mut B, tuple: &[String]) {
    VarInt::from_u64(tuple.len() as u64).unwrap().encode(buf);
    for element in tuple {
        encode_string(buf, element);
    }
}

fn decode_tuple<B: Buf>(buf: &mut B) -> anyhow::Result<TrackNamespace> {
    let num_elements = VarInt::decode(buf)?.into_inner() as usize;
    let mut elements = Vec::with_capacity(num_elements);
    for _ in 0..num_elements {
        elements.push(decode_string(buf)?);
    }
    Ok(elements)
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
                VarInt::from_u64(m.subscribe_id).unwrap().encode(&mut payload);
                VarInt::from_u64(m.track_alias).unwrap().encode(&mut payload);
                encode_tuple(&mut payload, &m.track_namespace);
                encode_string(&mut payload, &m.track_name);
                payload.put_u8(m.subscriber_priority);
                payload.put_u8(m.group_order);
                VarInt::from_u64(m.filter_type).unwrap().encode(&mut payload);
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::SubscribeOk(m) => {
                VarInt::from_u64(MSG_SUBSCRIBE_OK).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                VarInt::from_u64(m.subscribe_id).unwrap().encode(&mut payload);
                VarInt::from_u64(m.expires).unwrap().encode(&mut payload);
                payload.put_u8(m.group_order);
                payload.put_u8(if m.content_exists { 1 } else { 0 });
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::RequestError(m) => {
                VarInt::from_u64(MSG_REQUEST_ERROR).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                VarInt::from_u64(m.subscribe_id).unwrap().encode(&mut payload);
                VarInt::from_u64(m.error_code).unwrap().encode(&mut payload);
                encode_string(&mut payload, &m.reason_phrase);
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::Unsubscribe(m) => {
                VarInt::from_u64(MSG_UNSUBSCRIBE).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                VarInt::from_u64(m.subscribe_id).unwrap().encode(&mut payload);
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::SubscribeDone(m) => {
                VarInt::from_u64(MSG_SUBSCRIBE_DONE).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                VarInt::from_u64(m.subscribe_id).unwrap().encode(&mut payload);
                VarInt::from_u64(m.status_code).unwrap().encode(&mut payload);
                encode_string(&mut payload, &m.reason_phrase);
                payload.put_u8(if m.content_exists { 1 } else { 0 });
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::GoAway(m) => {
                VarInt::from_u64(MSG_GOAWAY).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                encode_string(&mut payload, &m.new_session_uri);
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::Publish(m) => {
                VarInt::from_u64(MSG_PUBLISH).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                encode_tuple(&mut payload, &m.track_namespace);
                encode_string(&mut payload, &m.track_name);
                payload.put_u8(m.subscriber_priority);
                payload.put_u8(m.group_order);
                VarInt::from_u64(0).unwrap().encode(&mut payload); // num_params=0
                buf.put_u16(payload.len() as u16);
                buf.put_slice(&payload);
            }
            Message::PublishOk(m) => {
                VarInt::from_u64(MSG_PUBLISH_OK).unwrap().encode(&mut buf);
                let mut payload = BytesMut::new();
                encode_tuple(&mut payload, &m.track_namespace);
                encode_string(&mut payload, &m.track_name);
                VarInt::from_u64(0).unwrap().encode(&mut payload); // num_params=0
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
                let subscribe_id = VarInt::decode(&mut payload)?.into_inner();
                let track_alias = VarInt::decode(&mut payload)?.into_inner();
                let track_namespace = decode_tuple(&mut payload)?;
                let track_name = decode_string(&mut payload)?;
                if payload.remaining() < 2 {
                    anyhow::bail!("not enough data for Subscribe priority/group_order");
                }
                let subscriber_priority = payload.get_u8();
                let group_order = payload.get_u8();
                let filter_type = VarInt::decode(&mut payload)?.into_inner();
                Ok(Message::Subscribe(Subscribe {
                    subscribe_id,
                    track_alias,
                    track_namespace,
                    track_name,
                    subscriber_priority,
                    group_order,
                    filter_type,
                }))
            }
            MSG_SUBSCRIBE_OK => {
                let subscribe_id = VarInt::decode(&mut payload)?.into_inner();
                let expires = VarInt::decode(&mut payload)?.into_inner();
                if payload.remaining() < 2 {
                    anyhow::bail!("not enough data for SubscribeOk group_order/content_exists");
                }
                let group_order = payload.get_u8();
                let content_exists = payload.get_u8() != 0;
                Ok(Message::SubscribeOk(SubscribeOk {
                    subscribe_id,
                    expires,
                    group_order,
                    content_exists,
                }))
            }
            MSG_REQUEST_ERROR => {
                let subscribe_id = VarInt::decode(&mut payload)?.into_inner();
                let error_code = VarInt::decode(&mut payload)?.into_inner();
                let reason_phrase = decode_string(&mut payload)?;
                Ok(Message::RequestError(RequestError {
                    subscribe_id,
                    error_code,
                    reason_phrase,
                }))
            }
            MSG_UNSUBSCRIBE => {
                let subscribe_id = VarInt::decode(&mut payload)?.into_inner();
                Ok(Message::Unsubscribe(Unsubscribe { subscribe_id }))
            }
            MSG_SUBSCRIBE_DONE => {
                let subscribe_id = VarInt::decode(&mut payload)?.into_inner();
                let status_code = VarInt::decode(&mut payload)?.into_inner();
                let reason_phrase = decode_string(&mut payload)?;
                if payload.remaining() < 1 {
                    anyhow::bail!("not enough data for SubscribeDone content_exists");
                }
                let content_exists = payload.get_u8() != 0;
                Ok(Message::SubscribeDone(SubscribeDone {
                    subscribe_id,
                    status_code,
                    reason_phrase,
                    content_exists,
                }))
            }
            MSG_GOAWAY => {
                let new_session_uri = decode_string(&mut payload)?;
                Ok(Message::GoAway(GoAway { new_session_uri }))
            }
            MSG_PUBLISH => {
                let track_namespace = decode_tuple(&mut payload)?;
                let track_name = decode_string(&mut payload)?;
                if payload.remaining() < 2 {
                    anyhow::bail!("not enough data for Publish priority/group_order");
                }
                let subscriber_priority = payload.get_u8();
                let group_order = payload.get_u8();
                let _num_params = VarInt::decode(&mut payload)?.into_inner();
                Ok(Message::Publish(Publish {
                    track_namespace,
                    track_name,
                    subscriber_priority,
                    group_order,
                }))
            }
            MSG_PUBLISH_OK => {
                let track_namespace = decode_tuple(&mut payload)?;
                let track_name = decode_string(&mut payload)?;
                let _num_params = VarInt::decode(&mut payload)?.into_inner();
                Ok(Message::PublishOk(PublishOk {
                    track_namespace,
                    track_name,
                }))
            }
            other => anyhow::bail!("unknown message type: 0x{other:x}"),
        }
    }
}

impl SubgroupHeader {
    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        VarInt::from_u64(self.subscribe_id).unwrap().encode(buf);
        VarInt::from_u64(self.track_alias).unwrap().encode(buf);
        VarInt::from_u64(self.group_id).unwrap().encode(buf);
        VarInt::from_u64(self.subgroup_id).unwrap().encode(buf);
        buf.put_u8(self.publisher_priority);
    }

    pub fn decode<B: Buf>(buf: &mut B) -> anyhow::Result<Self> {
        let subscribe_id = VarInt::decode(buf)?.into_inner();
        let track_alias = VarInt::decode(buf)?.into_inner();
        let group_id = VarInt::decode(buf)?.into_inner();
        let subgroup_id = VarInt::decode(buf)?.into_inner();
        if buf.remaining() < 1 {
            anyhow::bail!("not enough data for publisher_priority");
        }
        let publisher_priority = buf.get_u8();
        Ok(Self {
            subscribe_id,
            track_alias,
            group_id,
            subgroup_id,
            publisher_priority,
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
            subscribe_id: 1,
            track_alias: 2,
            track_namespace: vec!["ns".into()],
            track_name: "track".into(),
            subscriber_priority: 0,
            group_order: GROUP_ORDER_ASCENDING,
            filter_type: FILTER_NEXT_GROUP,
        });
        let encoded = msg.encode();
        let decoded = Message::decode(&mut encoded.clone()).unwrap();
        match decoded {
            Message::Subscribe(s) => {
                assert_eq!(s.subscribe_id, 1);
                assert_eq!(s.track_alias, 2);
                assert_eq!(s.track_namespace, vec!["ns".to_string()]);
                assert_eq!(s.track_name, "track");
                assert_eq!(s.subscriber_priority, 0);
                assert_eq!(s.group_order, GROUP_ORDER_ASCENDING);
                assert_eq!(s.filter_type, FILTER_NEXT_GROUP);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn roundtrip_subscribe_ok() {
        let msg = Message::SubscribeOk(SubscribeOk {
            subscribe_id: 5,
            expires: 0,
            group_order: GROUP_ORDER_ASCENDING,
            content_exists: false,
        });
        let encoded = msg.encode();
        let decoded = Message::decode(&mut encoded.clone()).unwrap();
        match decoded {
            Message::SubscribeOk(ok) => {
                assert_eq!(ok.subscribe_id, 5);
                assert_eq!(ok.expires, 0);
                assert_eq!(ok.group_order, GROUP_ORDER_ASCENDING);
                assert!(!ok.content_exists);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn roundtrip_publish() {
        let msg = Message::Publish(Publish {
            track_namespace: vec!["live".into()],
            track_name: "video".into(),
            subscriber_priority: 0,
            group_order: GROUP_ORDER_ASCENDING,
        });
        let encoded = msg.encode();
        let decoded = Message::decode(&mut encoded.clone()).unwrap();
        match decoded {
            Message::Publish(p) => {
                assert_eq!(p.track_namespace, vec!["live".to_string()]);
                assert_eq!(p.track_name, "video");
                assert_eq!(p.subscriber_priority, 0);
                assert_eq!(p.group_order, GROUP_ORDER_ASCENDING);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn roundtrip_publish_ok() {
        let msg = Message::PublishOk(PublishOk {
            track_namespace: vec!["live".into()],
            track_name: "video".into(),
        });
        let encoded = msg.encode();
        let decoded = Message::decode(&mut encoded.clone()).unwrap();
        match decoded {
            Message::PublishOk(ok) => {
                assert_eq!(ok.track_namespace, vec!["live".to_string()]);
                assert_eq!(ok.track_name, "video");
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn roundtrip_subgroup_header() {
        let hdr = SubgroupHeader {
            subscribe_id: 1,
            track_alias: 2,
            group_id: 3,
            subgroup_id: 0,
            publisher_priority: 128,
        };
        let mut buf = BytesMut::new();
        hdr.encode(&mut buf);
        let decoded = SubgroupHeader::decode(&mut buf.freeze()).unwrap();
        assert_eq!(decoded.subscribe_id, 1);
        assert_eq!(decoded.track_alias, 2);
        assert_eq!(decoded.group_id, 3);
        assert_eq!(decoded.subgroup_id, 0);
        assert_eq!(decoded.publisher_priority, 128);
    }

    #[test]
    fn roundtrip_goaway() {
        let msg = Message::GoAway(GoAway {
            new_session_uri: "https://new.example.com".into(),
        });
        let encoded = msg.encode();
        let decoded = Message::decode(&mut encoded.clone()).unwrap();
        match decoded {
            Message::GoAway(g) => assert_eq!(g.new_session_uri, "https://new.example.com"),
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn roundtrip_unsubscribe() {
        let msg = Message::Unsubscribe(Unsubscribe { subscribe_id: 42 });
        let encoded = msg.encode();
        let decoded = Message::decode(&mut encoded.clone()).unwrap();
        match decoded {
            Message::Unsubscribe(u) => assert_eq!(u.subscribe_id, 42),
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn roundtrip_request_error() {
        let msg = Message::RequestError(RequestError {
            subscribe_id: 1,
            error_code: 0x01,
            reason_phrase: "not found".into(),
        });
        let encoded = msg.encode();
        let decoded = Message::decode(&mut encoded.clone()).unwrap();
        match decoded {
            Message::RequestError(e) => {
                assert_eq!(e.subscribe_id, 1);
                assert_eq!(e.error_code, 0x01);
                assert_eq!(e.reason_phrase, "not found");
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn roundtrip_subscribe_done() {
        let msg = Message::SubscribeDone(SubscribeDone {
            subscribe_id: 3,
            status_code: 0,
            reason_phrase: "done".into(),
            content_exists: true,
        });
        let encoded = msg.encode();
        let decoded = Message::decode(&mut encoded.clone()).unwrap();
        match decoded {
            Message::SubscribeDone(d) => {
                assert_eq!(d.subscribe_id, 3);
                assert_eq!(d.status_code, 0);
                assert_eq!(d.reason_phrase, "done");
                assert!(d.content_exists);
            }
            _ => panic!("wrong message type"),
        }
    }
}
