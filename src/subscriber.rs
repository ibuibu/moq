use anyhow::Result;
use wtransport::Connection;

use crate::message::{
    Message, Subscribe, SubgroupHeader, TrackNamespace, Unsubscribe, FILTER_NEXT_GROUP,
    GROUP_ORDER_ASCENDING,
};
use crate::session::{recv_message, send_message};
use crate::varint::VarInt;

/// Subscriber: サーバーに SUBSCRIBE を送り、Object を受信する
#[allow(dead_code)]
pub struct Subscriber {
    connection: Connection,
    control_send: wtransport::stream::SendStream,
    control_recv: wtransport::stream::RecvStream,
    track_namespace: TrackNamespace,
    track_name: String,
    subscribe_id: u64,
    track_alias: u64,
}

impl Subscriber {
    /// SUBSCRIBE メッセージを送信し、SubscribeOk を受信する
    pub async fn new(
        connection: Connection,
        mut control_send: wtransport::stream::SendStream,
        mut control_recv: wtransport::stream::RecvStream,
        track_namespace: TrackNamespace,
        track_name: String,
        subscribe_id: u64,
        track_alias: u64,
    ) -> Result<Self> {
        let subscribe = Message::Subscribe(Subscribe {
            subscribe_id,
            track_alias,
            track_namespace: track_namespace.clone(),
            track_name: track_name.clone(),
            subscriber_priority: 0,
            group_order: GROUP_ORDER_ASCENDING,
            filter_type: FILTER_NEXT_GROUP,
        });
        send_message(&mut control_send, &subscribe).await?;
        tracing::info!("sent Subscribe for {:?}/{track_name}", track_namespace);

        let resp = recv_message(&mut control_recv).await?;
        match resp {
            Message::SubscribeOk(ok) => {
                if ok.subscribe_id != subscribe_id {
                    anyhow::bail!(
                        "subscribe_id mismatch: expected {}, got {}",
                        subscribe_id,
                        ok.subscribe_id
                    );
                }
                tracing::info!("received SubscribeOk subscribe_id={}", ok.subscribe_id);
            }
            other => anyhow::bail!("expected SubscribeOk, got {:?}", other),
        }

        Ok(Self {
            connection,
            control_send,
            control_recv,
            track_namespace,
            track_name,
            subscribe_id,
            track_alias,
        })
    }

    /// サーバーから Object を受信する (unidirectional stream を accept)
    /// 戻り値: (SubgroupHeader, object_id, payload)
    pub async fn recv_object(&self) -> Result<(SubgroupHeader, u64, Vec<u8>)> {
        let mut recv = self.connection.accept_uni().await?;

        let mut buf = Vec::new();
        let mut tmp = [0u8; 65536];
        loop {
            match recv.read(&mut tmp).await? {
                Some(n) => buf.extend_from_slice(&tmp[..n]),
                None => break,
            }
        }

        let mut data = bytes::Bytes::from(buf);
        let header = SubgroupHeader::decode(&mut data)?;
        let object_id = VarInt::decode(&mut data)?.into_inner();
        let payload_length = VarInt::decode(&mut data)?.into_inner() as usize;
        let payload = data[..payload_length].to_vec();

        tracing::debug!(
            "received object alias={} group={} obj={} len={}",
            header.track_alias,
            header.group_id,
            object_id,
            payload.len()
        );

        Ok((header, object_id, payload))
    }

    /// UNSUBSCRIBE メッセージを送信する
    pub async fn unsubscribe(&mut self) -> Result<()> {
        let msg = Message::Unsubscribe(Unsubscribe {
            subscribe_id: self.subscribe_id,
        });
        send_message(&mut self.control_send, &msg).await?;
        tracing::info!("sent Unsubscribe subscribe_id={}", self.subscribe_id);
        Ok(())
    }
}
