use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use bytes::BytesMut;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
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

    // WSL2 などで外部からアクセスする場合のホストIP を検出
    let host_ip = detect_host_ip();
    let san_names: Vec<String> = {
        let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
        if host_ip != "127.0.0.1" {
            names.push(host_ip.clone());
        }
        names
    };

    let identity = Identity::self_signed(san_names.iter().map(|s| s.as_str()))?;

    // 証明書の SHA-256 ハッシュを取得
    let cert_hash: [u8; 32] = *identity
        .certificate_chain()
        .as_slice()[0]
        .hash()
        .as_ref();
    tracing::info!(
        "certificate SHA-256: {}",
        cert_hash.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
    );

    // HTTP サーバー (HTML / JS 配信)
    let viewer_html = generate_viewer_html(&cert_hash, &host_ip);
    let publisher_html = generate_publisher_html(&cert_hash, &host_ip);
    tokio::spawn(async move {
        if let Err(e) = run_http_server(viewer_html, publisher_html).await {
            tracing::error!("HTTP server error: {e}");
        }
    });

    let config = ServerConfig::builder()
        .with_bind_default(4433)
        .with_identity(identity)
        .build();

    let server = Endpoint::server(config)?;
    let tracks: TrackChannels = Arc::new(Mutex::new(HashMap::new()));

    tracing::info!("MoQ server listening on :4433");
    tracing::info!("WebTransport host: {host_ip}");
    tracing::info!("Viewer:    http://localhost:8080");
    tracing::info!("Publisher: http://localhost:8080/publish");

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

async fn run_http_server(viewer_html: String, publisher_html: String) -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("HTTP server listening on http://localhost:8080");

    loop {
        let (mut stream, addr) = listener.accept().await?;
        let viewer_html = viewer_html.clone();
        let publisher_html = publisher_html.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let n = match tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await {
                Ok(n) => n,
                Err(_) => return,
            };

            // リクエスト行からパスを取得 (e.g. "GET /publish HTTP/1.1")
            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");

            let (content_type, body) = match path {
                "/publish" => ("text/html; charset=utf-8", publisher_html.as_str()),
                "/common.js" => ("application/javascript", include_str!("../../static/common.js")),
                "/viewer.js" => ("application/javascript", include_str!("../../static/viewer.js")),
                "/publisher.js" => ("application/javascript", include_str!("../../static/publisher.js")),
                _ => ("text/html; charset=utf-8", viewer_html.as_str()),
            };

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            if let Err(e) = stream.write_all(response.as_bytes()).await {
                tracing::warn!("HTTP write error for {addr}: {e}");
            }
        });
    }
}

/// デフォルト経路のローカル IP を取得（WSL2 環境で実 IP を検出）
///
/// WSL2 (NAT モード) では `localhost` 経由の UDP 転送が機能しないため、
/// WebTransport (QUIC/UDP) の接続先には WSL2 の実 IP が必要になる。
///
/// UDP ソケットに対して `connect()` を呼ぶと、実際にはパケットを送信せずに
/// OS のルーティングテーブルを参照して送信元 IP を決定する。
/// その後 `local_addr()` で取得できるアドレスがデフォルト経路のローカル IP になる。
fn detect_host_ip() -> String {
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                return addr.ip().to_string();
            }
        }
    }
    "127.0.0.1".to_string()
}

fn generate_viewer_html(cert_hash: &[u8; 32], host_ip: &str) -> String {
    let hash_array = cert_hash
        .iter()
        .map(|b| format!("0x{b:02x}"))
        .collect::<Vec<_>>()
        .join(", ");

    include_str!("../../static/viewer.html")
        .replace("__CERT_HASH__", &hash_array)
        .replace("__HOST_IP__", host_ip)
}

async fn forward_track(
    connection: wtransport::Connection,
    mut rx: broadcast::Receiver<(ObjectHeader, Vec<u8>)>,
) {
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

            // 最初の PublishOk を返す
            let ok = Message::PublishOk(PublishOk {
                track_namespace: p.track_namespace.clone(),
                track_name: p.track_name.clone(),
            });
            send_message(&mut control_send, &ok).await?;

            // 最初のトラックの broadcast チャネルを作成
            {
                let mut map = tracks.lock().await;
                let key = (p.track_namespace.clone(), p.track_name.clone());
                map.entry(key)
                    .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0);
            }

            // 制御ストリームをバックグラウンドで読み続け、追加 Publish に対応
            let tracks_ctrl = tracks.clone();
            tokio::spawn(async move {
                loop {
                    match recv_message(&mut control_recv).await {
                        Ok(Message::Publish(p2)) => {
                            tracing::info!("additional publish for {}/{}", p2.track_namespace, p2.track_name);
                            let ok = Message::PublishOk(PublishOk {
                                track_namespace: p2.track_namespace.clone(),
                                track_name: p2.track_name.clone(),
                            });
                            if send_message(&mut control_send, &ok).await.is_err() {
                                break;
                            }
                            let key = (p2.track_namespace.clone(), p2.track_name.clone());
                            let mut map = tracks_ctrl.lock().await;
                            map.entry(key)
                                .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0);
                        }
                        Ok(other) => {
                            tracing::warn!("unexpected message from publisher: {:?}", other);
                        }
                        Err(_) => break,
                    }
                }
            });

            // Publisher からの unidirectional stream を受け取って broadcast
            // ObjectHeader の namespace/name で適切なチャネルにルーティング
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

                // ObjectHeader の namespace/name に対応するチャネルに送信
                let key = (header.track_namespace.clone(), header.track_name.clone());
                let tx = {
                    let mut map = tracks.lock().await;
                    map.entry(key)
                        .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0)
                        .clone()
                };
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

            // 最初のトラック転送を spawn
            let rx = {
                let mut map = tracks.lock().await;
                let key = (s.track_namespace.clone(), s.track_name.clone());
                let tx = map
                    .entry(key)
                    .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0)
                    .clone();
                tx.subscribe()
            };
            let conn = connection.clone();
            tokio::spawn(forward_track(conn, rx));

            // 制御ストリームを読み続け、追加 Subscribe に対応
            loop {
                match recv_message(&mut control_recv).await {
                    Ok(Message::Subscribe(s2)) => {
                        tracing::info!("additional subscribe for {}/{}", s2.track_namespace, s2.track_name);
                        let ok = Message::SubscribeOk(SubscribeOk {
                            track_namespace: s2.track_namespace.clone(),
                            track_name: s2.track_name.clone(),
                        });
                        if send_message(&mut control_send, &ok).await.is_err() {
                            break;
                        }
                        let rx = {
                            let mut map = tracks.lock().await;
                            let key = (s2.track_namespace.clone(), s2.track_name.clone());
                            let tx = map
                                .entry(key)
                                .or_insert_with(|| broadcast::channel(BROADCAST_CAPACITY).0)
                                .clone();
                            tx.subscribe()
                        };
                        let conn = connection.clone();
                        tokio::spawn(forward_track(conn, rx));
                    }
                    Ok(other) => {
                        tracing::warn!("unexpected message from subscriber: {:?}", other);
                    }
                    Err(_) => break,
                }
            }
        }

        other => {
            anyhow::bail!("expected Publish or Subscribe, got {:?}", other);
        }
    }

    Ok(())
}

fn generate_publisher_html(cert_hash: &[u8; 32], host_ip: &str) -> String {
    let hash_array = cert_hash
        .iter()
        .map(|b| format!("0x{b:02x}"))
        .collect::<Vec<_>>()
        .join(", ");

    include_str!("../../static/publisher.html")
        .replace("__CERT_HASH__", &hash_array)
        .replace("__HOST_IP__", host_ip)
}
