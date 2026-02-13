use anyhow::Result;
use wtransport::Connection;

use crate::message::{Message, ObjectHeader, Subscribe};
use crate::session::{recv_message, send_message};

/// Subscriber: サーバーに SUBSCRIBE を送り、Object を受信する
#[allow(dead_code)]
pub struct Subscriber {
    connection: Connection,
    control_send: wtransport::stream::SendStream,
    control_recv: wtransport::stream::RecvStream,
    track_namespace: String,
    track_name: String,
}

impl Subscriber {
    /// SUBSCRIBE メッセージを送信し、SubscribeOk を受信する
    pub async fn new(
        connection: Connection,
        mut control_send: wtransport::stream::SendStream,
        mut control_recv: wtransport::stream::RecvStream,
        track_namespace: String,
        track_name: String,
    ) -> Result<Self> {
        let subscribe = Message::Subscribe(Subscribe {
            track_namespace: track_namespace.clone(),
            track_name: track_name.clone(),
        });
        send_message(&mut control_send, &subscribe).await?;
        tracing::info!("sent Subscribe for {track_namespace}/{track_name}");

        let resp = recv_message(&mut control_recv).await?;
        match resp {
            Message::SubscribeOk(ok) => {
                tracing::info!("received SubscribeOk for {}/{}", ok.track_namespace, ok.track_name);
            }
            other => anyhow::bail!("expected SubscribeOk, got {:?}", other),
        }

        Ok(Self {
            connection,
            control_send,
            control_recv,
            track_namespace,
            track_name,
        })
    }

    /// サーバーから Object を受信する (unidirectional stream を accept)
    pub async fn recv_object(&self) -> Result<(ObjectHeader, Vec<u8>)> {
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
        let header = ObjectHeader::decode(&mut data)?;
        let payload = data.to_vec();

        tracing::debug!(
            "received object {}/{} group={} id={} len={}",
            header.track_namespace,
            header.track_name,
            header.group_id,
            header.object_id,
            payload.len()
        );

        Ok((header, payload))
    }
}
