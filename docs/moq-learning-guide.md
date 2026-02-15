# MoQ (Media over QUIC Transport) 学習ガイド

この文書は、本リポジトリの実装 (draft-ietf-moq-transport-15 準拠) をもとに MoQ プロトコルの仕組みを学ぶためのガイドです。

---

## 目次

1. [MoQ とは何か](#1-moq-とは何か)
2. [プロトコルスタック](#2-プロトコルスタック)
3. [基本データ型: QUIC 可変長整数 (VarInt)](#3-基本データ型-quic-可変長整数-varint)
4. [メッセージフレーミング](#4-メッセージフレーミング)
5. [セッションライフサイクル](#5-セッションライフサイクル)
6. [Track と Namespace](#6-track-と-namespace)
7. [パブリッシュフロー (PUBLISH / PUBLISH_OK)](#7-パブリッシュフロー-publish--publish_ok)
8. [サブスクライブフロー (SUBSCRIBE / SUBSCRIBE_OK)](#8-サブスクライブフロー-subscribe--subscribe_ok)
9. [Object 配信モデル (Group / Subgroup / Object)](#9-object-配信モデル-group--subgroup--object)
10. [Unidirectional Stream とデータ転送](#10-unidirectional-stream-とデータ転送)
11. [ライフサイクルメッセージ](#11-ライフサイクルメッセージ)
12. [サーバーの中継アーキテクチャ](#12-サーバーの中継アーキテクチャ)
13. [ブラウザクライアントの実装](#13-ブラウザクライアントの実装)
14. [ワイヤフォーマット一覧](#14-ワイヤフォーマット一覧)
15. [全体シーケンス図](#15-全体シーケンス図)

---

## 1. MoQ とは何か

**MoQ (Media over QUIC Transport)** は、IETF で策定中のリアルタイムメディア配信プロトコルです。QUIC/WebTransport 上に構築され、低遅延なライブ配信を実現します。

### 従来技術との違い

| 特性 | RTMP/HLS | WebRTC | MoQ |
|------|----------|--------|-----|
| レイテンシ | 数秒〜数十秒 | 数百ms | 数百ms〜数秒 |
| スケーラビリティ | CDN OK | P2P中心 | CDN + Relay |
| トランスポート | TCP | UDP (DTLS/SRTP) | QUIC (UDP) |
| ブラウザ対応 | HTTP ベース | ネイティブ | WebTransport API |
| 中継サーバー | メディア変換が必要 | SFU/MCU | **バイト列をそのまま中継** |

MoQ の最大の特徴は **メディアのフォーマットに依存しない** こと。サーバーはペイロードの中身を一切解釈せず、バイナリデータとして受け取ってそのまま転送します。コーデック (H.264, Opus 等) の処理はエンドポイント (Publisher / Subscriber) だけが行います。

---

## 2. プロトコルスタック

```
┌─────────────────────────────────────┐
│        Application (H.264 / Opus)   │  ← WebCodecs (ブラウザ)
├─────────────────────────────────────┤
│        MoQ Transport (draft-15)     │  ← 本実装のメイン部分
├─────────────────────────────────────┤
│        WebTransport (HTTP/3)        │  ← wtransport crate
├─────────────────────────────────────┤
│        QUIC (RFC 9000)              │
├─────────────────────────────────────┤
│        UDP                          │
└─────────────────────────────────────┘
```

### 本実装の依存関係

- **Rust サーバー/クライアント**: `wtransport` 0.7 (WebTransport 実装)
- **ブラウザクライアント**: Chrome の `WebTransport` API + `WebCodecs` API

QUIC の特徴である **ストリーム多重化** と **Head-of-Line Blocking 回避** が、MoQ のリアルタイム性の鍵です。

---

## 3. 基本データ型: QUIC 可変長整数 (VarInt)

MoQ は **QUIC Variable-Length Integer** (RFC 9000 Section 16) を多用します。先頭 2 ビットでバイト長を判別する効率的な可変長エンコーディングです。

### エンコーディング規則

| 先頭 2bit | バイト長 | 有効ビット | 値の範囲 |
|-----------|---------|-----------|---------|
| `00` | 1 byte | 6 bit | 0 〜 63 |
| `01` | 2 bytes | 14 bit | 0 〜 16,383 |
| `10` | 4 bytes | 30 bit | 0 〜 1,073,741,823 |
| `11` | 8 bytes | 62 bit | 0 〜 4,611,686,018,427,387,903 |

### Rust 実装 (`src/varint.rs`)

```rust
// エンコード: 値の大きさで分岐
pub fn encode<B: BufMut>(&self, buf: &mut B) {
    let v = self.0;
    if v < (1 << 6) {
        buf.put_u8(v as u8);                  // 先頭 2bit = 00
    } else if v < (1 << 14) {
        buf.put_u16(0x4000 | v as u16);        // 先頭 2bit = 01
    } else if v < (1 << 30) {
        buf.put_u32(0x8000_0000 | v as u32);   // 先頭 2bit = 10
    } else {
        buf.put_u64(0xc000_0000_0000_0000 | v); // 先頭 2bit = 11
    }
}

// デコード: 先頭バイトの上位 2bit でバイト長を決定
pub fn decode<B: Buf>(buf: &mut B) -> anyhow::Result<Self> {
    let first = buf.chunk()[0];
    let prefix = first >> 6;       // 上位 2bit
    let len = 1usize << prefix;    // 1, 2, 4, or 8

    let mut val = (first & 0x3f) as u64;  // 下位 6bit を初期値に
    buf.advance(1);
    for _ in 1..len {
        val = (val << 8) | buf.get_u8() as u64;
    }
    Ok(Self(val))
}
```

### 具体例

値 `300` をエンコードする場合:
1. `300 < 16384` → 2 バイト形式 (先頭 2bit = `01`)
2. `0x4000 | 300` = `0x4000 | 0x012C` = `0x412C`
3. ワイヤ上: `[0x41, 0x2C]`

デコード:
1. `0x41` → 上位 2bit = `01` → 2 バイト
2. `0x41 & 0x3F` = `0x01`, 次のバイト `0x2C`
3. `(0x01 << 8) | 0x2C` = `300`

---

## 4. メッセージフレーミング

MoQ の制御メッセージは **bidirectional stream** 上を流れます。各メッセージのフレーム構造:

```
┌──────────────┬──────────────┬──────────────────────┐
│ Type (varint)│ Length (u16) │ Payload (Length bytes)│
└──────────────┴──────────────┴──────────────────────┘
```

- **Type**: メッセージ種別を表す VarInt
- **Length**: ペイロード長を u16 ビッグエンディアンで格納
- **Payload**: メッセージ固有のデータ

### メッセージタイプ ID 一覧 (draft-15)

| ID | 名前 | 方向 |
|----|------|------|
| `0x20` | CLIENT_SETUP | Client → Server |
| `0x21` | SERVER_SETUP | Server → Client |
| `0x03` | SUBSCRIBE | Subscriber → Server |
| `0x04` | SUBSCRIBE_OK | Server → Subscriber |
| `0x05` | REQUEST_ERROR | Server → Client |
| `0x0A` | UNSUBSCRIBE | Subscriber → Server |
| `0x0B` | SUBSCRIBE_DONE | Server → Subscriber |
| `0x10` | GOAWAY | 双方向 |
| `0x1D` | PUBLISH | Publisher → Server |
| `0x1E` | PUBLISH_OK | Server → Publisher |

### Rust 実装 (`src/message.rs:240-457`)

```rust
// エンコード (全メッセージ共通パターン)
pub fn encode(&self) -> Bytes {
    let mut buf = BytesMut::new();
    match self {
        Message::Subscribe(m) => {
            VarInt::from_u64(MSG_SUBSCRIBE).unwrap().encode(&mut buf);
            let mut payload = BytesMut::new();
            // ... payload にフィールドを書き込む ...
            buf.put_u16(payload.len() as u16);  // Length framing
            buf.put_slice(&payload);
        }
        // ...
    }
    buf.freeze()
}

// デコード
pub fn decode(buf: &mut Bytes) -> anyhow::Result<Self> {
    let msg_type = VarInt::decode(buf)?.into_inner();
    let length = buf.get_u16() as usize;           // Length
    let mut payload = buf.split_to(length);         // Payload
    match msg_type {
        MSG_SUBSCRIBE => { /* ... */ }
        // ...
    }
}
```

### JavaScript 実装 (`frontend/common.js:153-171`)

ブラウザ側では `readMessage()` がストリームからデータを逐次読み込み、メッセージが完成するまでバッファリングします:

```javascript
async function readMessage(reader, existingBuf) {
  let buf = existingBuf || new Uint8Array(0);
  while (true) {
    try {
      const msg = decodeMessage(buf);  // パース試行
      const remaining = buf.slice(msg.totalBytes);
      return { msg, remaining };
    } catch (e) {
      // データ不足 → もっと読む
    }
    const { done, value } = await reader.read();
    if (done) throw new Error('stream closed');
    // buf に追記して再試行
  }
}
```

---

## 5. セッションライフサイクル

MoQ セッションは以下の手順で確立されます:

```
Client                              Server
  │                                   │
  │─── WebTransport 接続 ────────────→│
  │                                   │
  │═══ Bidirectional Stream 開設 ═════│
  │                                   │
  │─── CLIENT_SETUP ────────────────→│
  │                                   │
  │←── SERVER_SETUP ────────────────│
  │                                   │
  │   (以降、この制御ストリームで      │
  │    PUBLISH/SUBSCRIBE 等を送受信)  │
```

### SETUP パラメータ

SETUP メッセージはキー・バリュー形式のパラメータを持てます:

| Key | 名前 | 値の型 |
|-----|------|--------|
| `0x0` | Role | VarInt |
| `0x1` | Path | String |
| `0x2` | MaxSubscribeId | VarInt |

パラメータのワイヤフォーマット:
```
varint(key) varint(value_length) value_bytes
```

### Rust 実装 (`src/session.rs`)

```rust
// クライアント側: bi-directional stream を開いて SETUP 交換
pub async fn client_setup(connection: &Connection)
    -> Result<(SendStream, RecvStream)>
{
    let (mut send, mut recv) = connection.open_bi().await?.await?;
    //                                           ^^^^^^^^^^^^^^^^^^
    //        wtransport の open_bi() は 2段階 await が必要
    //        1回目: Opening を取得、2回目: 実際の stream を取得

    let setup = Message::ClientSetup(ClientSetup::default());
    send_message(&mut send, &setup).await?;

    let resp = recv_message(&mut recv).await?;
    match resp {
        Message::ServerSetup(_) => { /* OK */ }
        other => bail!("expected ServerSetup, got {:?}", other),
    }
    Ok((send, recv))
}

// サーバー側: bi-directional stream を受け入れて SETUP 交換
pub async fn server_setup(connection: &Connection)
    -> Result<(SendStream, RecvStream)>
{
    let (mut send, mut recv) = connection.accept_bi().await?;
    //                                                ^^^^^^^^
    //        accept_bi() は 1段階 await のみ

    let msg = recv_message(&mut recv).await?;
    match msg {
        Message::ClientSetup(_) => {
            let resp = Message::ServerSetup(ServerSetup::default());
            send_message(&mut send, &resp).await?;
        }
        other => bail!("expected ClientSetup, got {:?}", other),
    }
    Ok((send, recv))
}
```

> **ポイント**: `open_bi().await?.await?` vs `accept_bi().await?` — wtransport の API では、stream を **開く**側は 2 段階の await (Opening → Stream)、**受ける**側は 1 段階です。

---

## 6. Track と Namespace

MoQ ではメディアを **Track** という単位で管理します。

### Track の識別

Track は **(Namespace, Name)** のペアで一意に識別されます。

- **Namespace**: N-tuple of byte strings (draft-15 形式)
  - 例: `["live"]` (1 要素タプル)
  - 例: `["conference", "room1"]` (2 要素タプル)
- **Name**: 単一の文字列
  - 例: `"video"`, `"audio"`

### Namespace のワイヤフォーマット

```
varint(num_elements) [ varint(element_length) element_bytes ] ...
```

例: `["live"]` のエンコード:
```
0x01            ← num_elements = 1
0x04            ← element_length = 4
0x6C 0x69 0x76 0x65  ← "live" (UTF-8)
```

### Rust 型定義 (`src/message.rs:18`)

```rust
pub type TrackNamespace = Vec<String>;
```

### 本実装のトラック構成

| Track Alias | Namespace | Name | 内容 |
|-------------|-----------|------|------|
| 0 | `["live"]` | `"video"` | H.264 映像 (AnnexB, 640x480, 15fps) |
| 1 | `["live"]` | `"audio"` | Opus 音声 (48kHz, mono) |

**Track Alias** は、Object 転送時に Namespace+Name の代わりに使う短い数値識別子です。PUBLISH/SUBSCRIBE の送信順序で暗黙的に割り当てられます。

---

## 7. パブリッシュフロー (PUBLISH / PUBLISH_OK)

Publisher はメディアデータを送信する前に、制御ストリームで PUBLISH ハンドシェイクを行います。

```
Publisher                           Server
  │                                   │
  │─── PUBLISH (video) ────────────→│
  │←── PUBLISH_OK ─────────────────│
  │                                   │
  │─── PUBLISH (audio) ────────────→│
  │←── PUBLISH_OK ─────────────────│
  │                                   │
  │ (以降、uni stream で Object 送信) │
```

### PUBLISH メッセージ (0x1D) のワイヤフォーマット

```
track_namespace(tuple) | track_name(string)
| subscriber_priority(u8) | group_order(u8)
| num_params(varint)
```

### PUBLISH_OK メッセージ (0x1E) のワイヤフォーマット

```
track_namespace(tuple) | track_name(string)
| num_params(varint)
```

### Rust 実装 (`src/publisher.rs:23-58`)

```rust
pub async fn new(
    connection: Connection,
    mut control_send: SendStream,
    mut control_recv: RecvStream,
    track_namespace: TrackNamespace,
    track_name: String,
    track_alias: u64,
) -> Result<Self> {
    // PUBLISH メッセージを送信
    let publish = Message::Publish(Publish {
        track_namespace: track_namespace.clone(),
        track_name: track_name.clone(),
        subscriber_priority: 0,
        group_order: GROUP_ORDER_ASCENDING,
    });
    send_message(&mut control_send, &publish).await?;

    // PUBLISH_OK を受信
    let resp = recv_message(&mut control_recv).await?;
    match resp {
        Message::PublishOk(ok) => { /* 成功 */ }
        other => bail!("expected PublishOk, got {:?}", other),
    }

    Ok(Self { /* ... */ track_alias, group_id: 0, object_id: 0 })
}
```

---

## 8. サブスクライブフロー (SUBSCRIBE / SUBSCRIBE_OK)

Subscriber はトラックの受信を開始する前に SUBSCRIBE ハンドシェイクを行います。

```
Subscriber                          Server
  │                                   │
  │─── SUBSCRIBE (video) ──────────→│
  │←── SUBSCRIBE_OK ───────────────│
  │                                   │
  │─── SUBSCRIBE (audio) ──────────→│
  │←── SUBSCRIBE_OK ───────────────│
  │                                   │
  │ (以降、uni stream で Object 受信) │
```

### SUBSCRIBE メッセージ (0x03) のワイヤフォーマット

```
subscribe_id(varint) | track_alias(varint)
| track_namespace(tuple) | track_name(string)
| subscriber_priority(u8) | group_order(u8)
| filter_type(varint)
```

フィールドの意味:

| フィールド | 説明 |
|-----------|------|
| `subscribe_id` | この Subscription を識別する ID (クライアントが割り当て) |
| `track_alias` | Object 転送時にトラックを識別する短い数値 |
| `subscriber_priority` | 優先度 (0 = 最高) |
| `group_order` | `0x01` = 昇順 (古い Group から配信) |
| `filter_type` | `0x01` = FILTER_NEXT_GROUP (次の Group から配信開始) |

### SUBSCRIBE_OK メッセージ (0x04) のワイヤフォーマット

```
subscribe_id(varint) | expires(varint)
| group_order(u8) | content_exists(u8)
```

> **注目**: SUBSCRIBE_OK には Namespace や Name が含まれません。`subscribe_id` でどの SUBSCRIBE に対する応答かを対応付けます。

### サーバー側の処理 (`src/bin/moq-server.rs` — `handle_subscriber()`)

サーバーは SUBSCRIBE を受け取ると:
1. `SubscribeOk` を返す
2. `broadcast::channel` の受信側 (`rx`) を取得
3. `forward_track()` タスクを spawn して Object を転送開始

```rust
Message::Subscribe(s) => {
    // 1. SubscribeOk を返す
    let ok = Message::SubscribeOk(SubscribeOk {
        subscribe_id: s.subscribe_id,
        expires: 0,
        group_order: GROUP_ORDER_ASCENDING,
        content_exists: false,
    });
    send_message(&mut control_send, &ok).await?;

    // 2. broadcast channel を subscribe
    let rx = {
        let mut map = tracks.lock().await;
        let tx = map.entry(key).or_insert_with(|| broadcast::channel(512).0);
        tx.subscribe()
    };

    // 3. 転送タスクを起動
    tokio::spawn(forward_track(conn, s.subscribe_id, s.track_alias, rx));
}
```

---

## 9. Object 配信モデル (Group / Subgroup / Object)

MoQ のメディアデータは階層構造で管理されます:

```
Track
 └── Group 0
 │    └── Subgroup 0
 │         ├── Object 0 (key frame)
 │         ├── Object 1 (delta frame)
 │         └── Object 2 (delta frame)
 └── Group 1
      └── Subgroup 0
           ├── Object 0 (key frame)
           ├── Object 1 (delta frame)
           └── Object 2 (delta frame)
```

### 各レベルの役割

| レベル | 説明 | 本実装での使い方 |
|--------|------|-----------------|
| **Group** | 独立してデコード可能なデータ単位 | GOP (Group of Pictures)。キーフレームから始まる |
| **Subgroup** | Group 内の細分化 | 常に `0` (未使用) |
| **Object** | 最小のデータ単位 | 1 映像フレーム or 1 音声パケット |

### 映像の例

```
Group 0: [KeyFrame, DeltaFrame, DeltaFrame, ..., DeltaFrame]  (30フレーム)
Group 1: [KeyFrame, DeltaFrame, DeltaFrame, ..., DeltaFrame]  (30フレーム)
```

キーフレーム間隔は `KEY_FRAME_INTERVAL = 30` (= 15fps × 2秒)。新しい Subscriber は次のキーフレーム (= Group の先頭) からデコードを開始できます。

### 音声の場合

音声では Group の概念は使わず、全 Object が `group_id=0` に属します。Opus は各フレームが独立してデコード可能なため、どこからでも再生開始できます。

---

## 10. Unidirectional Stream とデータ転送

制御メッセージは bidirectional stream を使いますが、メディアデータ (Object) は **unidirectional stream** で転送します。

### なぜ unidirectional stream を使うか

1. **独立した QUIC stream** = 個別のフロー制御と輻輳制御
2. **Head-of-Line Blocking 回避**: 1 つの Object のロスが他の Object をブロックしない
3. **Priority**: QUIC レイヤでストリームの優先度を設定可能

### STREAM_HEADER_SUBGROUP フォーマット

各 unidirectional stream の先頭には SubgroupHeader が付きます:

```
┌─────────────────────────────────────────────────────────┐
│ subscribe_id(varint) | track_alias(varint)              │
│ group_id(varint)     | subgroup_id(varint)              │
│ publisher_priority(u8)                                  │
├─────────────────────────────────────────────────────────┤
│ object_id(varint)    | payload_length(varint)           │
│ payload(payload_length bytes)                           │
└─────────────────────────────────────────────────────────┘
 ↑ SubgroupHeader        ↑ Object Entry
```

### Publisher 側: Object 送信 (`src/publisher.rs:61-90`)

```rust
pub async fn send_object(&mut self, payload: &[u8]) -> Result<()> {
    let header = SubgroupHeader {
        subscribe_id: 0,
        track_alias: self.track_alias,
        group_id: self.group_id,
        subgroup_id: 0,
        publisher_priority: 0,
    };

    // 1. Unidirectional stream を開く
    let mut send = self.connection.open_uni().await?.await?;

    // 2. ヘッダ + Object entry を書き込む
    let mut buf = BytesMut::new();
    header.encode(&mut buf);
    VarInt::from_u64(self.object_id).unwrap().encode(&mut buf);
    VarInt::from_u64(payload.len() as u64).unwrap().encode(&mut buf);
    send.write_all(&buf).await?;
    send.write_all(payload).await?;

    // 3. ストリームを完了
    send.finish().await?;
    self.object_id += 1;
    Ok(())
}
```

### Subscriber 側: Object 受信 (`src/subscriber.rs:73-92`)

```rust
pub async fn recv_object(&self) -> Result<(SubgroupHeader, u64, Vec<u8>)> {
    // 1. Unidirectional stream を受け入れ
    let mut recv = self.connection.accept_uni().await?;

    // 2. ストリーム全体を読み込み (session.rs の共通ヘルパー)
    let buf = read_entire_stream(&mut recv).await?;

    // 3. SubgroupHeader + Object entry をデコード
    let mut data = Bytes::from(buf);
    let header = SubgroupHeader::decode(&mut data)?;
    let object_id = VarInt::decode(&mut data)?.into_inner();
    let payload_length = VarInt::decode(&mut data)?.into_inner() as usize;
    let payload = data[..payload_length].to_vec();

    Ok((header, object_id, payload))
}
```

### 1 Object = 1 Stream

本実装では **1 つの Object ごとに 1 つの unidirectional stream** を使います。ストリームを開いてヘッダ+ペイロードを書き込み、すぐに `finish()` で閉じます。

> **設計判断**: draft-15 では 1 つの stream に複数の Object を連続して書くことも可能ですが、本実装では 1:1 マッピングを採用してシンプルにしています。

---

## 11. ライフサイクルメッセージ

セッション中に送受信されるライフサイクル管理メッセージ:

### GOAWAY (0x10)

サーバーがシャットダウンする際に送信。クライアントは指定された新しい URI に再接続できます。

```
new_session_uri(string)
```

### UNSUBSCRIBE (0x0A)

Subscriber がトラックの受信を停止する際に送信。

```
subscribe_id(varint)
```

### REQUEST_ERROR (0x05)

SUBSCRIBE や PUBLISH に対するエラー応答。

```
subscribe_id(varint) | error_code(varint) | reason_phrase(string)
```

### SUBSCRIBE_DONE (0x0B)

サーバーがサブスクリプションの終了を通知。

```
subscribe_id(varint) | status_code(varint) | reason_phrase(string) | content_exists(u8)
```

---

## 12. サーバーの中継アーキテクチャ

サーバー (`src/bin/moq-server.rs`) は **Publisher → Server → Subscriber** の中継を行います。

### データフロー

```
Publisher                    Server                     Subscriber
    │                          │                           │
    │── uni stream ──────→ [broadcast::channel] ──→ uni stream ──→│
    │   (SubgroupHeader       per Track                    │
    │    + Object)                                         │
```

### Track ルーティング

サーバーはグローバルな `HashMap<TrackKey, broadcast::Sender<RelayObject>>` を保持します:

```rust
type TrackKey = (TrackNamespace, String);  // (["live"], "video")
type TrackChannels = Arc<Mutex<HashMap<TrackKey, broadcast::Sender<RelayObject>>>>;
```

### Publisher 側: track_alias → TrackKey マッピング

Publisher が PUBLISH メッセージを送る順番で track_alias が決まります:

```
PUBLISH ["live"]/"video"  → publisher_tracks[0] = (["live"], "video")   → alias=0
PUBLISH ["live"]/"audio"  → publisher_tracks[1] = (["live"], "audio")   → alias=1
```

Object 受信時は `header.track_alias` でルーティング:

```rust
let alias = header.track_alias as usize;
let key = publisher_tracks[alias].clone();  // alias → TrackKey
let tx = tracks[key].clone();               // TrackKey → broadcast::Sender
tx.send(RelayObject { ... });               // broadcast で全 Subscriber に配信
```

### Subscriber 側: broadcast → forward_track()

各 Subscriber の各トラックに対して `forward_track()` タスクが spawn されます:

```rust
async fn forward_track(
    connection: Connection,
    subscribe_id: u64,    // この Subscriber 固有の ID
    track_alias: u64,     // この Subscriber 固有の alias
    mut rx: broadcast::Receiver<RelayObject>,
) {
    loop {
        let obj = rx.recv().await?;

        // Subscriber 向けに SubgroupHeader を書き換えて転送
        let header = SubgroupHeader {
            subscribe_id,   // Subscriber の subscribe_id
            track_alias,    // Subscriber の track_alias
            group_id: obj.group_id,
            subgroup_id: obj.subgroup_id,
            publisher_priority: obj.publisher_priority,
        };

        let mut send = connection.open_uni().await?.await?;
        // ヘッダ + Object entry + payload を書いて close
    }
}
```

> **重要**: Publisher の `subscribe_id`/`track_alias` と Subscriber の `subscribe_id`/`track_alias` は **異なる値** です。サーバーが中継時に書き換えます。

### broadcast channel の挙動

- `broadcast::channel(512)` で容量 512 のチャネルを作成
- Subscriber が遅れると `RecvError::Lagged(n)` が返り、古いメッセージがスキップされる
- これにより、遅い Subscriber が Publisher や他の Subscriber をブロックしない

---

## 13. ブラウザクライアントの実装

ブラウザクライアントは 3 つの JS ファイルで構成されます:

| ファイル | 役割 |
|---------|------|
| `frontend/common.js` | VarInt, String, Tuple, ClientSetup のエンコード/デコード |
| `frontend/publisher.js` | カメラ映像 + マイク音声 → WebCodecs → MoQ 送信 |
| `frontend/viewer.js` | MoQ 受信 → WebCodecs → Canvas 描画 + 音声再生 |

### 設定の取得

ブラウザクライアントは起動時に moq-server の `/config` API から接続情報を取得します:

```javascript
// common.js の fetchConfig()
async function fetchConfig() {
  const res = await fetch('/config');
  const config = await res.json();
  CERT_HASH = new Uint8Array(config.certHash);
  HOST_IP = config.hostIp;
}
```

開発時は Vite dev server のプロキシ経由で `/config` → `http://localhost:8080/config` に転送されます。

### Publisher (ブラウザ): エンコード → 送信

```
Camera/Mic → MediaStream → WebCodecs (VideoEncoder/AudioEncoder)
                                          ↓
                              EncodedVideoChunk / EncodedAudioChunk
                                          ↓
                              SubgroupHeader + payload
                                          ↓
                              WebTransport unidirectional stream
```

映像のエンコード設定:
```javascript
encoder.configure({
    codec: 'avc1.42001f',   // H.264 Baseline Profile Level 3.1
    width: 640, height: 480,
    bitrate: 1_000_000,     // 1 Mbps
    framerate: 15,
    avc: { format: 'annexb' },  // AnnexB 形式 (start code 付き)
});
```

音声のエンコード設定:
```javascript
audioEncoder.configure({
    codec: 'opus',
    sampleRate: 48000,
    numberOfChannels: 1,
    bitrate: 64000,  // 64 kbps
});
```

### Viewer (ブラウザ): 受信 → デコード

```
WebTransport incomingUnidirectionalStreams
        ↓
    readStream() → 全バイト受信
        ↓
    decodeSubgroupStream() → trackAlias でルーティング
        ↓
    ┌─ trackAlias=0 (video) → EncodedVideoChunk → VideoDecoder → Canvas
    └─ trackAlias=1 (audio) → EncodedAudioChunk → AudioDecoder → AudioContext
```

キーフレーム待ち処理:
```javascript
const isKey = header.objectId === 0;  // Group の先頭 Object = キーフレーム
if (!receivedKeyFrame) {
    if (!isKey) {
        console.log('skipping delta frame (waiting for key frame)');
        return;
    }
    receivedKeyFrame = true;
}
```

音声再生 (ギャップレス):
```javascript
// AudioBufferSourceNode でスケジューリング
const source = audioContext.createBufferSource();
source.buffer = buffer;
source.connect(audioContext.destination);
if (nextAudioTime < currentTime) {
    nextAudioTime = currentTime;  // 遅れた場合は現在時刻にリセット
}
source.start(nextAudioTime);
nextAudioTime += numFrames / sampleRate;  // 次の再生開始時刻
```

---

## 14. ワイヤフォーマット一覧

### 制御メッセージ (bidirectional stream)

#### CLIENT_SETUP (0x20)
```
varint(0x20) u16(length)
  varint(num_params) [ varint(key) varint(value_len) value_bytes ] ...
```

#### SERVER_SETUP (0x21)
```
varint(0x21) u16(length)
  varint(num_params) [ varint(key) varint(value_len) value_bytes ] ...
```

#### SUBSCRIBE (0x03)
```
varint(0x03) u16(length)
  varint(subscribe_id) varint(track_alias)
  tuple(track_namespace) string(track_name)
  u8(subscriber_priority) u8(group_order)
  varint(filter_type)
```

#### SUBSCRIBE_OK (0x04)
```
varint(0x04) u16(length)
  varint(subscribe_id) varint(expires)
  u8(group_order) u8(content_exists)
```

#### PUBLISH (0x1D)
```
varint(0x1D) u16(length)
  tuple(track_namespace) string(track_name)
  u8(subscriber_priority) u8(group_order)
  varint(num_params=0)
```

#### PUBLISH_OK (0x1E)
```
varint(0x1E) u16(length)
  tuple(track_namespace) string(track_name)
  varint(num_params=0)
```

#### REQUEST_ERROR (0x05)
```
varint(0x05) u16(length)
  varint(subscribe_id) varint(error_code)
  string(reason_phrase)
```

#### UNSUBSCRIBE (0x0A)
```
varint(0x0A) u16(length)
  varint(subscribe_id)
```

#### SUBSCRIBE_DONE (0x0B)
```
varint(0x0B) u16(length)
  varint(subscribe_id) varint(status_code)
  string(reason_phrase) u8(content_exists)
```

#### GOAWAY (0x10)
```
varint(0x10) u16(length)
  string(new_session_uri)
```

### データストリーム (unidirectional stream)

#### STREAM_HEADER_SUBGROUP + Object Entry
```
varint(subscribe_id) varint(track_alias)
varint(group_id) varint(subgroup_id)
u8(publisher_priority)
varint(object_id) varint(payload_length)
payload[payload_length]
```

---

## 15. 全体シーケンス図

Publisher と Subscriber が同時に接続する場合の全体フロー:

```
Publisher            Server              Subscriber
    │                  │                     │
    │── WT Connect ──→│                     │
    │                  │←── WT Connect ─────│
    │                  │                     │
    │══ open_bi() ════│                     │
    │── ClientSetup ─→│                     │
    │←─ ServerSetup ──│                     │
    │                  │                     │
    │                  │══ accept_bi() ═════│
    │                  │←── ClientSetup ────│
    │                  │──→ ServerSetup ────│
    │                  │                     │
    │── PUBLISH ─────→│                     │
    │   (video)        │                     │
    │←─ PUBLISH_OK ───│                     │
    │                  │                     │
    │── PUBLISH ─────→│                     │
    │   (audio)        │                     │
    │←─ PUBLISH_OK ───│                     │
    │                  │                     │
    │                  │←── SUBSCRIBE ──────│
    │                  │    (video)          │
    │                  │──→ SUBSCRIBE_OK ──→│
    │                  │                     │
    │                  │←── SUBSCRIBE ──────│
    │                  │    (audio)          │
    │                  │──→ SUBSCRIBE_OK ──→│
    │                  │                     │
    │                  │   [broadcast ch     │
    │                  │    created per      │
    │                  │    track]           │
    │                  │                     │
    │── uni stream ──→│── uni stream ──────→│
    │   (video key)    │   (video key)       │  ← VideoDecoder
    │                  │                     │
    │── uni stream ──→│── uni stream ──────→│
    │   (video delta)  │   (video delta)     │
    │                  │                     │
    │── uni stream ──→│── uni stream ──────→│
    │   (audio opus)   │   (audio opus)      │  ← AudioDecoder
    │                  │                     │
    │    ...           │    ...              │
    │                  │                     │
    │                  │←── UNSUBSCRIBE ────│  (任意)
    │                  │                     │
    │                  │──→ GOAWAY ────────→│  (シャットダウン時)
```

---

## 補足: 実行方法

```bash
# サーバー起動
cargo run --bin moq-server

# ブラウザクライアント起動 (別ターミナル)
cd frontend && npm install && npm run dev

# ブラウザで開く
# Viewer:    http://localhost:5173
# Publisher: http://localhost:5173/publisher.html
```

Chrome で Publisher ページを開いてカメラを許可し、"Start Publishing" をクリック。
別タブで Viewer ページを開くと映像と音声がリアルタイムで再生されます。

カメラがない環境ではテストパターン (カラーバー + 動くボックス) が自動的に使われます。

---

## 参考資料

- [draft-ietf-moq-transport-15](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/) — MoQ Transport 仕様書
- [RFC 9000](https://www.rfc-editor.org/rfc/rfc9000) — QUIC Transport Protocol
- [WebTransport API](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport) — ブラウザ API
- [WebCodecs API](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API) — 映像/音声コーデック API
