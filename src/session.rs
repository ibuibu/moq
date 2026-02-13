use anyhow::Result;
use bytes::BytesMut;
use wtransport::Connection;

use crate::message::{ClientSetup, Message, ServerSetup};

const READ_BUF_SIZE: usize = 65536;

/// Control stream 上でメッセージを送受信するヘルパー
pub async fn send_message(
    send: &mut wtransport::stream::SendStream,
    msg: &Message,
) -> Result<()> {
    let data = msg.encode();
    send.write_all(&data).await?;
    Ok(())
}

pub async fn recv_message(recv: &mut wtransport::stream::RecvStream) -> Result<Message> {
    let mut buf = BytesMut::with_capacity(READ_BUF_SIZE);
    let mut tmp = [0u8; READ_BUF_SIZE];
    loop {
        let n = recv
            .read(&mut tmp)
            .await?
            .ok_or_else(|| anyhow::anyhow!("stream closed"))?;
        buf.extend_from_slice(&tmp[..n]);

        let mut data = buf.clone().freeze();
        match Message::decode(&mut data) {
            Ok(msg) => {
                let consumed = buf.len() - data.remaining();
                let _ = buf.split_to(consumed);
                return Ok(msg);
            }
            Err(_) => continue, // まだデータ不足、もっと読む
        }
    }
}

use bytes::Buf;

/// クライアント側 SETUP ハンドシェイク
pub async fn client_setup(connection: &Connection) -> Result<(wtransport::stream::SendStream, wtransport::stream::RecvStream)> {
    let (mut send, mut recv) = connection.open_bi().await?.await?;

    let setup = Message::ClientSetup(ClientSetup::default());
    send_message(&mut send, &setup).await?;
    tracing::info!("sent ClientSetup");

    let resp = recv_message(&mut recv).await?;
    match resp {
        Message::ServerSetup(ss) => {
            tracing::info!("received ServerSetup: version=0x{:x}", ss.selected_version);
        }
        other => anyhow::bail!("expected ServerSetup, got {:?}", other),
    }

    Ok((send, recv))
}

/// サーバー側 SETUP ハンドシェイク
pub async fn server_setup(connection: &Connection) -> Result<(wtransport::stream::SendStream, wtransport::stream::RecvStream)> {
    let (mut send, mut recv) = connection.accept_bi().await?;

    let msg = recv_message(&mut recv).await?;
    match msg {
        Message::ClientSetup(cs) => {
            tracing::info!("received ClientSetup: versions={:?}", cs.supported_versions);
            // 簡易バージョンネゴシエーション: 最初のバージョンを選択
            let selected = cs.supported_versions.first().copied().unwrap_or(0);
            let resp = Message::ServerSetup(ServerSetup {
                selected_version: selected,
            });
            send_message(&mut send, &resp).await?;
            tracing::info!("sent ServerSetup: version=0x{selected:x}");
        }
        other => anyhow::bail!("expected ClientSetup, got {:?}", other),
    }

    Ok((send, recv))
}
