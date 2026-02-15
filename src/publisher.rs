use anyhow::Result;
use bytes::BytesMut;
use wtransport::Connection;

use crate::message::{Message, Publish, SubgroupHeader, TrackNamespace, GROUP_ORDER_ASCENDING};
use crate::session::{recv_message, send_message};
use crate::varint::VarInt;

/// Publisher: サーバーに PUBLISH を送り、Object を unidirectional stream で送信する
#[allow(dead_code)]
pub struct Publisher {
    connection: Connection,
    control_send: wtransport::stream::SendStream,
    control_recv: wtransport::stream::RecvStream,
    track_namespace: TrackNamespace,
    track_name: String,
    track_alias: u64,
    group_id: u64,
    object_id: u64,
}

impl Publisher {
    /// PUBLISH メッセージを送信し、PublishOk を受信する
    pub async fn new(
        connection: Connection,
        mut control_send: wtransport::stream::SendStream,
        mut control_recv: wtransport::stream::RecvStream,
        track_namespace: TrackNamespace,
        track_name: String,
        track_alias: u64,
    ) -> Result<Self> {
        let publish = Message::Publish(Publish {
            track_namespace: track_namespace.clone(),
            track_name: track_name.clone(),
            subscriber_priority: 0,
            group_order: GROUP_ORDER_ASCENDING,
        });
        send_message(&mut control_send, &publish).await?;
        tracing::info!("sent Publish for {:?}/{track_name}", track_namespace);

        let resp = recv_message(&mut control_recv).await?;
        match resp {
            Message::PublishOk(ok) => {
                tracing::info!("received PublishOk for {:?}/{}", ok.track_namespace, ok.track_name);
            }
            other => anyhow::bail!("expected PublishOk, got {:?}", other),
        }

        Ok(Self {
            connection,
            control_send,
            control_recv,
            track_namespace,
            track_name,
            track_alias,
            group_id: 0,
            object_id: 0,
        })
    }

    /// Object をサーバーに送信する (unidirectional stream を開いて送る)
    pub async fn send_object(&mut self, payload: &[u8]) -> Result<()> {
        let header = SubgroupHeader {
            subscribe_id: 0,
            track_alias: self.track_alias,
            group_id: self.group_id,
            subgroup_id: 0,
            publisher_priority: 0,
        };

        let mut send = self.connection.open_uni().await?.await?;

        let mut buf = BytesMut::new();
        header.encode(&mut buf);
        // object entry: object_id + payload_length + payload
        VarInt::from_u64(self.object_id).unwrap().encode(&mut buf);
        VarInt::from_u64(payload.len() as u64).unwrap().encode(&mut buf);
        send.write_all(&buf).await?;
        send.write_all(payload).await?;
        send.finish().await?;

        tracing::debug!(
            "sent object group={} id={} len={}",
            self.group_id,
            self.object_id,
            payload.len()
        );

        self.object_id += 1;
        Ok(())
    }

    pub fn next_group(&mut self) {
        self.group_id += 1;
        self.object_id = 0;
    }
}
