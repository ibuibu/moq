use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use bytes::BytesMut;
use tokio::sync::broadcast;
use tokio::sync::Mutex;
use wtransport::Endpoint;
use wtransport::Identity;
use wtransport::ServerConfig;

use moq::message::{Message, ObjectHeader, PublishOk, SubscribeOk};
use moq::session::{recv_message, send_message, server_setup};

/// Track ごとの broadcast チャネル
type TrackKey = (String, String); // (namespace, name)
type TrackChannels = Arc<Mutex<HashMap<TrackKey, broadcast::Sender<(ObjectHeader, Vec<u8>)>>>>;

const BROADCAST_CAPACITY: usize = 512;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"])?;
    let config = ServerConfig::builder()
        .with_bind_default(4433)
        .with_identity(identity)
        .build();

    let server = Endpoint::server(config)?;
    let tracks: TrackChannels = Arc::new(Mutex::new(HashMap::new()));

    tracing::info!("MoQ server listening on :4433");

    loop {
        let incoming = server.accept().await;
        let tracks = tracks.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_session(incoming, tracks).await {
                tracing::error!("session error: {e}");
            }
        });
    }
}

async fn handle_session(
    incoming: wtransport::endpoint::IncomingSession,
    tracks: TrackChannels,
) -> Result<()> {
    let session_request = incoming.await?;
    tracing::info!("new session from {}", session_request.remote_address());

    let connection = session_request.accept().await?;
    let (mut control_send, mut control_recv) = server_setup(&connection).await?;

    // 次のメッセージで Publisher か Subscriber かを判別
    let msg = recv_message(&mut control_recv).await?;
    match msg {
        Message::Publish(p) => {
            tracing::info!("publisher for {}/{}", p.track_namespace, p.track_name);

            // PublishOk を返す
            let ok = Message::PublishOk(PublishOk {
                track_namespace: p.track_namespace.clone(),
                track_name: p.track_name.clone(),
            });
            send_message(&mut control_send, &ok).await?;

            // broadcast チャネルを取得 or 作成
            let key = (p.track_namespace.clone(), p.track_name.clone());
            let tx = {
                let mut map = tracks.lock().await;
                map.entry(key)
                    .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0)
                    .clone()
            };

            // Publisher からの unidirectional stream を受け取って broadcast
            loop {
                let mut recv = match connection.accept_uni().await {
                    Ok(r) => r,
                    Err(_) => break,
                };

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

                tracing::info!(
                    "relaying object {}/{} group={} id={} len={}",
                    header.track_namespace,
                    header.track_name,
                    header.group_id,
                    header.object_id,
                    payload.len()
                );

                // 受信者がいなくてもエラーにはしない
                let _ = tx.send((header, payload));
            }
        }

        Message::Subscribe(s) => {
            tracing::info!("subscriber for {}/{}", s.track_namespace, s.track_name);

            // SubscribeOk を返す
            let ok = Message::SubscribeOk(SubscribeOk {
                track_namespace: s.track_namespace.clone(),
                track_name: s.track_name.clone(),
            });
            send_message(&mut control_send, &ok).await?;

            // broadcast チャネルを購読
            let key = (s.track_namespace.clone(), s.track_name.clone());
            let mut rx = {
                let mut map = tracks.lock().await;
                let tx = map
                    .entry(key)
                    .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0)
                    .clone();
                tx.subscribe()
            };

            // Object を subscriber に中継 (unidirectional stream で送る)
            loop {
                let (header, payload) = match rx.recv().await {
                    Ok(v) => v,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("subscriber lagged, skipped {n} messages");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };

                let mut send = match connection.open_uni().await {
                    Ok(opening) => match opening.await {
                        Ok(s) => s,
                        Err(_) => break,
                    },
                    Err(_) => break,
                };

                let mut buf = BytesMut::new();
                header.encode(&mut buf);
                if send.write_all(&buf).await.is_err() {
                    break;
                }
                if send.write_all(&payload).await.is_err() {
                    break;
                }
                let _ = send.finish().await;
            }
        }

        other => {
            anyhow::bail!("expected Publish or Subscribe, got {:?}", other);
        }
    }

    Ok(())
}
