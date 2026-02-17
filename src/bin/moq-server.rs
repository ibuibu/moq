use std::sync::Arc;

use anyhow::Result;
use bytes::BytesMut;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use wtransport::Endpoint;
use wtransport::Identity;
use wtransport::ServerConfig;

use moq::message::{Message, PublishOk, SubgroupHeader, SubscribeOk, TrackNamespace, GROUP_ORDER_ASCENDING};
use moq::session::{read_entire_stream, recv_message, send_message, server_setup};
use moq::varint::VarInt;

/// 中継用オブジェクト (Redis Pub/Sub 経由で MessagePack シリアライズ)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RelayObject {
    group_id: u64,
    object_id: u64,
    subgroup_id: u64,
    publisher_priority: u8,
    #[serde(with = "serde_bytes")]
    payload: Vec<u8>,
}

/// Track の namespace + name から Redis チャネル名を生成
fn track_channel_name(namespace: &[String], track_name: &str) -> String {
    format!("moq:track:{}:{}", namespace.join("/"), track_name)
}

/// Track のキー (publisher_tracks のマッピング用)
type TrackKey = (TrackNamespace, String); // (namespace, name)

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

    // ポート設定 (環境変数で上書き可能)
    let quic_port: u16 = std::env::var("QUIC_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4433);
    let http_port: u16 = std::env::var("HTTP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8080);

    // HTTP サーバー (/config API)
    let cert_hash_vec = cert_hash.to_vec();
    let host_ip_clone = host_ip.clone();
    tokio::spawn(async move {
        if let Err(e) = run_http_server(cert_hash_vec, host_ip_clone, quic_port, http_port).await {
            tracing::error!("HTTP server error: {e}");
        }
    });

    let config = ServerConfig::builder()
        .with_bind_default(quic_port)
        .with_identity(identity)
        .build();

    let server = Endpoint::server(config)?;

    // Redis 接続
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1/".to_string());
    let redis_client = redis::Client::open(redis_url.as_str())?;
    // 起動時に接続確認
    let mut conn = redis_client.get_multiplexed_async_connection().await?;
    redis::cmd("PING").query_async::<String>(&mut conn).await?;
    tracing::info!("connected to Redis at {redis_url}");

    tracing::info!("MoQ server listening on :{quic_port}");
    tracing::info!("WebTransport host: {host_ip}");
    tracing::info!("Config API: http://localhost:{http_port}/config");

    loop {
        let incoming = server.accept().await;
        let redis_client = redis_client.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_session(incoming, redis_client).await {
                tracing::error!("session error: {e}");
            }
        });
    }
}

async fn run_http_server(cert_hash: Vec<u8>, host_ip: String, quic_port: u16, http_port: u16) -> Result<()> {
    let listener = TcpListener::bind(format!("0.0.0.0:{http_port}")).await?;
    tracing::info!("HTTP server listening on http://localhost:{http_port} (/config)");

    let config_json = format!(
        r#"{{"certHash":[{}],"hostIp":"{}","port":{}}}"#,
        cert_hash.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(","),
        host_ip,
        quic_port,
    );

    loop {
        let (mut stream, addr) = listener.accept().await?;
        let config_json = config_json.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let n = match tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await {
                Ok(n) => n,
                Err(_) => return,
            };

            let request = String::from_utf8_lossy(&buf[..n]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");

            let response = if path == "/config" {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    config_json.len(),
                    config_json
                )
            } else {
                let body = "404 Not Found";
                format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            };

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


async fn forward_track_redis(
    connection: wtransport::Connection,
    subscribe_id: u64,
    track_alias: u64,
    redis_client: redis::Client,
    channel_name: String,
) {
    let mut pubsub = match redis_client.get_async_pubsub().await {
        Ok(ps) => ps,
        Err(e) => {
            tracing::error!("failed to create Redis PubSub connection: {e}");
            return;
        }
    };
    if let Err(e) = pubsub.subscribe(&channel_name).await {
        tracing::error!("failed to subscribe to {channel_name}: {e}");
        return;
    }
    tracing::info!("Redis subscribed to {channel_name}");

    let mut stream = pubsub.on_message();
    while let Some(msg) = stream.next().await {
        let data: Vec<u8> = match msg.get_payload() {
            Ok(d) => d,
            Err(_) => continue,
        };
        let obj: RelayObject = match rmp_serde::from_slice(&data) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("failed to deserialize RelayObject: {e}");
                continue;
            }
        };

        let mut send = match connection.open_uni().await {
            Ok(opening) => match opening.await {
                Ok(s) => s,
                Err(_) => break,
            },
            Err(_) => break,
        };

        // SubgroupHeader を書き出す
        let header = SubgroupHeader {
            subscribe_id,
            track_alias,
            group_id: obj.group_id,
            subgroup_id: obj.subgroup_id,
            publisher_priority: obj.publisher_priority,
        };
        let mut buf = BytesMut::new();
        header.encode(&mut buf);
        // object entry
        VarInt::from_u64(obj.object_id).unwrap().encode(&mut buf);
        VarInt::from_u64(obj.payload.len() as u64).unwrap().encode(&mut buf);
        if send.write_all(&buf).await.is_err() {
            break;
        }
        if send.write_all(&obj.payload).await.is_err() {
            break;
        }
        let _ = send.finish().await;
    }
}

async fn handle_session(
    incoming: wtransport::endpoint::IncomingSession,
    redis_client: redis::Client,
) -> Result<()> {
    let session_request = incoming.await?;
    tracing::info!("new session from {}", session_request.remote_address());

    let connection = session_request.accept().await?;
    let (control_send, mut control_recv) = server_setup(&connection).await?;

    // 次のメッセージで Publisher か Subscriber かを判別
    let msg = recv_message(&mut control_recv).await?;
    match msg {
        Message::Publish(p) => {
            handle_publisher(connection, control_send, control_recv, p, redis_client).await
        }
        Message::Subscribe(s) => {
            handle_subscriber(connection, control_send, control_recv, s, redis_client).await
        }
        other => {
            anyhow::bail!("expected Publish or Subscribe, got {:?}", other);
        }
    }
}

async fn handle_publisher(
    connection: wtransport::Connection,
    mut control_send: wtransport::stream::SendStream,
    mut control_recv: wtransport::stream::RecvStream,
    p: moq::message::Publish,
    redis_client: redis::Client,
) -> Result<()> {
    tracing::info!("publisher for {:?}/{}", p.track_namespace, p.track_name);

    // 最初の PublishOk を返す
    let ok = Message::PublishOk(PublishOk {
        track_namespace: p.track_namespace.clone(),
        track_name: p.track_name.clone(),
    });
    send_message(&mut control_send, &ok).await?;

    // publisher_tracks: track_alias (= index) → TrackKey マッピング
    let mut publisher_tracks: Vec<TrackKey> = Vec::new();
    publisher_tracks.push((p.track_namespace.clone(), p.track_name.clone()));

    // 制御ストリームをバックグラウンドで読み続け、追加 Publish に対応
    let publisher_tracks_shared = Arc::new(Mutex::new(publisher_tracks));
    let publisher_tracks_ctrl = publisher_tracks_shared.clone();
    tokio::spawn(async move {
        loop {
            match recv_message(&mut control_recv).await {
                Ok(Message::Publish(p2)) => {
                    tracing::info!("additional publish for {:?}/{}", p2.track_namespace, p2.track_name);
                    let ok = Message::PublishOk(PublishOk {
                        track_namespace: p2.track_namespace.clone(),
                        track_name: p2.track_name.clone(),
                    });
                    if send_message(&mut control_send, &ok).await.is_err() {
                        break;
                    }
                    let key = (p2.track_namespace.clone(), p2.track_name.clone());
                    publisher_tracks_ctrl.lock().await.push(key);
                }
                Ok(Message::GoAway(g)) => {
                    tracing::info!("received GoAway from publisher: {}", g.new_session_uri);
                }
                Ok(other) => {
                    tracing::warn!("unexpected message from publisher: {:?}", other);
                }
                Err(_) => break,
            }
        }
    });

    // Redis 接続 (publish 用)
    let mut redis_conn = redis_client.get_multiplexed_async_connection().await?;

    // Publisher からの unidirectional stream を受け取って Redis に publish
    loop {
        let mut recv = match connection.accept_uni().await {
            Ok(r) => r,
            Err(_) => break,
        };

        let buf = read_entire_stream(&mut recv).await?;

        let mut data = bytes::Bytes::from(buf);
        let header = SubgroupHeader::decode(&mut data)?;
        let object_id = VarInt::decode(&mut data)?.into_inner();
        let payload_length = VarInt::decode(&mut data)?.into_inner() as usize;
        let payload = data[..payload_length].to_vec();

        // track_alias → TrackKey でルーティング
        let alias = header.track_alias as usize;
        let key = {
            let pt = publisher_tracks_shared.lock().await;
            if alias < pt.len() {
                pt[alias].clone()
            } else {
                tracing::warn!("unknown track_alias {alias}");
                continue;
            }
        };

        tracing::info!(
            "relaying object {:?}/{} group={} obj={} len={}",
            key.0,
            key.1,
            header.group_id,
            object_id,
            payload.len()
        );

        let channel = track_channel_name(&key.0, &key.1);
        let obj = RelayObject {
            group_id: header.group_id,
            object_id,
            subgroup_id: header.subgroup_id,
            publisher_priority: header.publisher_priority,
            payload,
        };
        let serialized = rmp_serde::to_vec(&obj)?;
        redis::cmd("PUBLISH")
            .arg(&channel)
            .arg(serialized)
            .query_async::<()>(&mut redis_conn)
            .await?;
    }

    Ok(())
}

async fn handle_subscriber(
    connection: wtransport::Connection,
    mut control_send: wtransport::stream::SendStream,
    mut control_recv: wtransport::stream::RecvStream,
    s: moq::message::Subscribe,
    redis_client: redis::Client,
) -> Result<()> {
    tracing::info!("subscriber for {:?}/{}", s.track_namespace, s.track_name);

    // SubscribeOk を返す
    let ok = Message::SubscribeOk(SubscribeOk {
        subscribe_id: s.subscribe_id,
        expires: 0,
        group_order: GROUP_ORDER_ASCENDING,
        content_exists: false,
    });
    send_message(&mut control_send, &ok).await?;

    // 最初のトラック転送を spawn
    let channel_name = track_channel_name(&s.track_namespace, &s.track_name);
    let conn = connection.clone();
    tokio::spawn(forward_track_redis(
        conn,
        s.subscribe_id,
        s.track_alias,
        redis_client.clone(),
        channel_name,
    ));

    // 制御ストリームを読み続け、追加 Subscribe に対応
    loop {
        match recv_message(&mut control_recv).await {
            Ok(Message::Subscribe(s2)) => {
                tracing::info!("additional subscribe for {:?}/{}", s2.track_namespace, s2.track_name);
                let ok = Message::SubscribeOk(SubscribeOk {
                    subscribe_id: s2.subscribe_id,
                    expires: 0,
                    group_order: GROUP_ORDER_ASCENDING,
                    content_exists: false,
                });
                if send_message(&mut control_send, &ok).await.is_err() {
                    break;
                }
                let channel_name = track_channel_name(&s2.track_namespace, &s2.track_name);
                let conn = connection.clone();
                tokio::spawn(forward_track_redis(
                    conn,
                    s2.subscribe_id,
                    s2.track_alias,
                    redis_client.clone(),
                    channel_name,
                ));
            }
            Ok(Message::Unsubscribe(u)) => {
                tracing::info!("received Unsubscribe subscribe_id={}", u.subscribe_id);
            }
            Ok(Message::GoAway(g)) => {
                tracing::info!("received GoAway from subscriber: {}", g.new_session_uri);
            }
            Ok(other) => {
                tracing::warn!("unexpected message from subscriber: {:?}", other);
            }
            Err(_) => break,
        }
    }

    Ok(())
}

