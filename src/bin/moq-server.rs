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

    // HTTP サーバー (ビューアー HTML 配信)
    let html = generate_viewer_html(&cert_hash, &host_ip);
    tokio::spawn(async move {
        if let Err(e) = run_http_server(html).await {
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
    tracing::info!("Open http://localhost:8080 in Chrome to view the stream");

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

async fn run_http_server(html: String) -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("HTTP server listening on http://localhost:8080");

    loop {
        let (mut stream, addr) = listener.accept().await?;
        let html = html.clone();
        tokio::spawn(async move {
            // リクエストを読み捨て
            let mut buf = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            if let Err(e) = stream.write_all(response.as_bytes()).await {
                tracing::warn!("HTTP write error for {addr}: {e}");
            }
        });
    }
}

/// デフォルト経路のローカル IP を取得（WSL2 環境で実 IP を検出）
fn detect_host_ip() -> String {
    // UDP ソケットを "接続" すると OS がルーティングを解決してくれる（実際には何も送らない）
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

    let host = host_ip;
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>MoQ Video Viewer</title>
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{
    background: #1a1a2e; color: #eee; font-family: monospace;
    display: flex; flex-direction: column; align-items: center;
    min-height: 100vh; padding: 20px;
  }}
  h1 {{ margin-bottom: 10px; color: #0ff; }}
  #status {{ margin-bottom: 10px; font-size: 14px; color: #aaa; }}
  #stats {{ margin-bottom: 10px; font-size: 13px; color: #888; }}
  #frame {{
    max-width: 90vw; max-height: 70vh;
    border: 2px solid #333; background: #000;
  }}
  .connected {{ color: #0f0 !important; }}
  .error {{ color: #f44 !important; }}
</style>
</head>
<body>
<h1>MoQ Video Viewer</h1>
<div id="status">Connecting...</div>
<div id="stats"></div>
<img id="frame" alt="video frame">

<script>
const CERT_HASH = new Uint8Array([{hash_array}]);

// --- VarInt (RFC 9000 Section 16) ---
function encodeVarInt(value) {{
  if (value < 0x40) {{
    return new Uint8Array([value]);
  }} else if (value < 0x4000) {{
    const buf = new Uint8Array(2);
    new DataView(buf.buffer).setUint16(0, 0x4000 | value);
    return buf;
  }} else if (value < 0x40000000) {{
    const buf = new Uint8Array(4);
    new DataView(buf.buffer).setUint32(0, 0x80000000 | value);
    return buf;
  }} else {{
    const buf = new Uint8Array(8);
    const dv = new DataView(buf.buffer);
    // value を上位32bit / 下位32bit に分割
    const hi = Math.floor(value / 0x100000000);
    const lo = value >>> 0;
    dv.setUint32(0, 0xc0000000 | hi);
    dv.setUint32(4, lo);
    return buf;
  }}
}}

function decodeVarInt(buf, offset) {{
  const first = buf[offset];
  const prefix = first >> 6;
  const len = 1 << prefix;
  if (offset + len > buf.length) throw new Error('not enough data for varint');

  let val = first & 0x3f;
  for (let i = 1; i < len; i++) {{
    val = val * 256 + buf[offset + i];
  }}
  return {{ value: val, bytesRead: len }};
}}

// --- String encode/decode ---
function encodeString(str) {{
  const encoded = new TextEncoder().encode(str);
  const lenBytes = encodeVarInt(encoded.length);
  const result = new Uint8Array(lenBytes.length + encoded.length);
  result.set(lenBytes);
  result.set(encoded, lenBytes.length);
  return result;
}}

function decodeString(buf, offset) {{
  const {{ value: len, bytesRead }} = decodeVarInt(buf, offset);
  offset += bytesRead;
  if (offset + len > buf.length) throw new Error('not enough data for string');
  const str = new TextDecoder().decode(buf.slice(offset, offset + len));
  return {{ value: str, bytesRead: bytesRead + len }};
}}

// --- MoQ Messages ---
function encodeClientSetup() {{
  const msgType = encodeVarInt(0x40); // CLIENT_SETUP
  const numVersions = encodeVarInt(1);
  const version = encodeVarInt(0xff000001); // draft version
  const numParams = encodeVarInt(0);
  return concatBytes([msgType, numVersions, version, numParams]);
}}

function encodeSubscribe(namespace, name) {{
  const msgType = encodeVarInt(0x03); // SUBSCRIBE
  const ns = encodeString(namespace);
  const nm = encodeString(name);
  return concatBytes([msgType, ns, nm]);
}}

function concatBytes(arrays) {{
  const totalLen = arrays.reduce((sum, a) => sum + a.length, 0);
  const result = new Uint8Array(totalLen);
  let offset = 0;
  for (const a of arrays) {{
    result.set(a, offset);
    offset += a.length;
  }}
  return result;
}}

// --- Decode messages from control stream ---
function decodeMessage(buf) {{
  let offset = 0;
  const {{ value: msgType, bytesRead: b1 }} = decodeVarInt(buf, offset);
  offset += b1;

  if (msgType === 0x41) {{ // SERVER_SETUP
    const {{ value: version, bytesRead: b2 }} = decodeVarInt(buf, offset);
    offset += b2;
    const {{ value: numParams, bytesRead: b3 }} = decodeVarInt(buf, offset);
    offset += b3;
    for (let i = 0; i < numParams; i++) {{
      const {{ bytesRead: kb }} = decodeVarInt(buf, offset);
      offset += kb;
      const {{ bytesRead: vb }} = decodeString(buf, offset);
      offset += vb;
    }}
    return {{ type: 'ServerSetup', version, totalBytes: offset }};
  }}

  if (msgType === 0x04) {{ // SUBSCRIBE_OK
    const {{ value: ns, bytesRead: b2 }} = decodeString(buf, offset);
    offset += b2;
    const {{ value: name, bytesRead: b3 }} = decodeString(buf, offset);
    offset += b3;
    return {{ type: 'SubscribeOk', namespace: ns, name, totalBytes: offset }};
  }}

  throw new Error('unknown message type: 0x' + msgType.toString(16));
}}

// --- ObjectHeader decode ---
function decodeObjectHeader(buf) {{
  let offset = 0;
  const {{ value: ns, bytesRead: b1 }} = decodeString(buf, offset);
  offset += b1;
  const {{ value: name, bytesRead: b2 }} = decodeString(buf, offset);
  offset += b2;
  const {{ value: groupId, bytesRead: b3 }} = decodeVarInt(buf, offset);
  offset += b3;
  const {{ value: objectId, bytesRead: b4 }} = decodeVarInt(buf, offset);
  offset += b4;
  const {{ value: payloadLength, bytesRead: b5 }} = decodeVarInt(buf, offset);
  offset += b5;
  return {{ ns, name, groupId, objectId, payloadLength, headerSize: offset }};
}}

// --- Main ---
const statusEl = document.getElementById('status');
const statsEl = document.getElementById('stats');
const frameEl = document.getElementById('frame');

let frameCount = 0;
let fpsStartTime = performance.now();
let prevUrl = null;

function setStatus(text, cls) {{
  statusEl.textContent = text;
  statusEl.className = cls || '';
}}

async function readStream(reader) {{
  const chunks = [];
  let totalLen = 0;
  while (true) {{
    const {{ done, value }} = await reader.read();
    if (done) break;
    chunks.push(value);
    totalLen += value.length;
  }}
  const result = new Uint8Array(totalLen);
  let offset = 0;
  for (const chunk of chunks) {{
    result.set(chunk, offset);
    offset += chunk.length;
  }}
  return result;
}}

async function readMessage(reader, existingBuf) {{
  let buf = existingBuf || new Uint8Array(0);

  while (true) {{
    try {{
      const msg = decodeMessage(buf);
      const remaining = buf.slice(msg.totalBytes);
      return {{ msg, remaining }};
    }} catch (e) {{
      // need more data
    }}
    const {{ done, value }} = await reader.read();
    if (done) throw new Error('stream closed while reading message');
    const newBuf = new Uint8Array(buf.length + value.length);
    newBuf.set(buf);
    newBuf.set(value, buf.length);
    buf = newBuf;
  }}
}}

async function start() {{
  try {{
    setStatus('Connecting to WebTransport...');
    const transport = new WebTransport('https://{host}:4433', {{
      serverCertificateHashes: [{{
        algorithm: 'sha-256',
        value: CERT_HASH.buffer,
      }}],
    }});
    await transport.ready;
    setStatus('Connected! Setting up...', 'connected');

    // 制御ストリーム (bidirectional)
    const bidi = await transport.createBidirectionalStream();
    const writer = bidi.writable.getWriter();
    const reader = bidi.readable.getReader();

    // ClientSetup 送信
    await writer.write(encodeClientSetup());

    // ServerSetup 受信
    let {{ msg: serverSetup, remaining }} = await readMessage(reader);
    if (serverSetup.type !== 'ServerSetup') {{
      throw new Error('Expected ServerSetup, got ' + serverSetup.type);
    }}
    console.log('ServerSetup received, version=0x' + serverSetup.version.toString(16));

    // Subscribe 送信
    await writer.write(encodeSubscribe('live', 'video'));

    // SubscribeOk 受信
    let result = await readMessage(reader, remaining);
    if (result.msg.type !== 'SubscribeOk') {{
      throw new Error('Expected SubscribeOk, got ' + result.msg.type);
    }}
    console.log('SubscribeOk received for ' + result.msg.namespace + '/' + result.msg.name);
    setStatus('Receiving video...', 'connected');

    // オブジェクト受信ループ (unidirectional streams)
    const incomingReader = transport.incomingUnidirectionalStreams.getReader();
    while (true) {{
      const {{ done, value: stream }} = await incomingReader.read();
      if (done) break;

      // 各ストリームを非同期で処理
      handleObjectStream(stream).catch(e => {{
        console.warn('stream error:', e);
      }});
    }}

    setStatus('Stream ended', '');
  }} catch (e) {{
    console.error(e);
    setStatus('Error: ' + e.message, 'error');
  }}
}}

async function handleObjectStream(stream) {{
  const data = await readStream(stream.getReader());
  const header = decodeObjectHeader(data);
  const payload = data.slice(header.headerSize, header.headerSize + header.payloadLength);

  // JPEG → img 表示
  const blob = new Blob([payload], {{ type: 'image/jpeg' }});
  const url = URL.createObjectURL(blob);
  if (prevUrl) URL.revokeObjectURL(prevUrl);
  prevUrl = url;
  frameEl.src = url;

  // Stats 更新
  frameCount++;
  const elapsed = (performance.now() - fpsStartTime) / 1000;
  const fps = elapsed > 0 ? (frameCount / elapsed).toFixed(1) : '0';
  statsEl.textContent = `Frames: ${{frameCount}} | FPS: ${{fps}} | Size: ${{payload.length}} bytes`;

  // 5秒ごとにFPSカウンタリセット
  if (elapsed > 5) {{
    frameCount = 0;
    fpsStartTime = performance.now();
  }}
}}

start();
</script>
</body>
</html>"#
    )
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
