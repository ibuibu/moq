# moq

Media over QUIC Transport (MoQ) の最小限実装。WebTransport 上でリアルタイム映像・音声をやり取りするクライアント・サーバーを Rust で実装している。

## 概要

- **moq-server** — WebTransport サーバー + `/config` API。Publisher からの映像/音声を Redis Pub/Sub 経由で Subscriber に中継する
- **moq-client** — CLI クライアント。テキスト/映像の publish/subscribe をサポート
- **ブラウザクライアント** — WebTransport API を使ったブラウザベースの Publisher/Viewer

## アーキテクチャ

### シングルサーバー

```
Browser Publisher ──┐                  ┌── Browser Viewer
                    ├─ WebTransport ─► moq-server ─┤
CLI Publisher ──────┘   (QUIC/UDP)     └── CLI Subscriber
                                           │
                                         Redis (Pub/Sub)
```

### 複数サーバー (Redis Pub/Sub で中継)

```
Publisher ─► moq-server A ─► Redis ◄─ moq-server B ◄─ Viewer
              :4433/:8080              :4434/:8081
```

Publisher と Viewer が別々のサーバーに接続しても、Redis Pub/Sub を介してリアルタイムにデータが中継される。

## プロジェクト構成

```
src/
  lib.rs             # ライブラリルート
  varint.rs          # QUIC 可変長整数 (RFC 9000 Section 16)
  message.rs         # MoQ メッセージ (ClientSetup, Subscribe, Publish, ObjectHeader, etc.)
  session.rs         # SETUP ハンドシェイク・メッセージ送受信ヘルパー
  publisher.rs       # Publisher (Object 送信)
  subscriber.rs      # Subscriber (Object 受信)
  bin/
    moq-server.rs    # サーバー (Redis Pub/Sub 中継 + /config API)
    moq-client.rs    # CLI クライアント (publish/subscribe/video-publish/video-subscribe)
frontend/
  index.html         # ブラウザ Viewer (H.264 + Opus デコード)
  publisher.html     # ブラウザ Publisher (H.264 + Opus エンコード)
  common.js          # 共有ユーティリティ (VarInt, String encode/decode, fetchConfig, etc.)
  viewer.js          # Viewer 固有ロジック
  publisher.js       # Publisher 固有ロジック
  package.json       # Vite 開発環境
  vite.config.js     # Vite 設定 (/config プロキシ)
docker-compose.yml   # Redis 起動用
```

## セットアップ

### 必須

- Rust (edition 2024)
- Docker (Redis 用)
- SDL2 開発ライブラリ (CLI video-subscribe に必要)

```bash
# Ubuntu/Debian
sudo apt install libsdl2-dev

# macOS
brew install sdl2
```

### ビルド

```bash
cargo build
```

## 使い方

### 1. Redis 起動

```bash
docker compose up -d
```

### 2. サーバー起動

```bash
cargo run --bin moq-server
```

以下が起動する:
- WebTransport サーバー: `:4433`
- Config API: `http://localhost:8080/config`

### 3. ブラウザクライアント

サーバー起動後、Vite dev server を起動して Chrome でアクセス:

```bash
cd frontend && npm install && npm run dev
```

- Viewer: http://localhost:5173
- Publisher: http://localhost:5173/publisher.html

自己署名証明書のハッシュと接続先 IP は `/config` API から自動取得される。

### CLI クライアント

```bash
# テキスト publish (stdin から入力)
cargo run --bin moq-client -- publish

# テキスト subscribe
cargo run --bin moq-client -- subscribe

# テストパターン映像 publish
cargo run --bin moq-client -- video-publish

# SDL2 ウィンドウで映像 subscribe
cargo run --bin moq-client -- video-subscribe
```

## 複数サーバー構成

Redis Pub/Sub により、複数の moq-server 間でデータを中継できる。`QUIC_PORT` と `HTTP_PORT` 環境変数でポートを指定する。

```bash
# Redis 起動
docker compose up -d

# サーバー A (デフォルト: QUIC=4433, HTTP=8080)
cargo run --bin moq-server

# サーバー B (別ターミナル)
QUIC_PORT=4434 HTTP_PORT=8081 cargo run --bin moq-server
```

ブラウザクライアントは `?port=` クエリパラメータで接続先サーバーの HTTP ポートを指定する:

```bash
# Publisher → サーバー A (デフォルト)
http://localhost:5173/publisher.html

# Viewer → サーバー B
http://localhost:5173/?port=8081
```

## Redis のデータ構造

Redis Pub/Sub のみを使用し、永続化データは持たない (Redis が落ちても再起動すれば復帰する)。

### チャネル名

```
moq:track:{namespace}:{track_name}
```

例:
- `moq:track:live:video` — 映像トラック
- `moq:track:live:audio` — 音声トラック

### メッセージ形式

各チャネルに publish されるメッセージは `RelayObject` を MessagePack でシリアライズしたバイナリ:

| フィールド           | 型      | 説明                              |
|---------------------|---------|-----------------------------------|
| `group_id`          | `u64`   | グループ ID (映像のキーフレーム単位) |
| `object_id`         | `u64`   | グループ内のオブジェクト ID          |
| `subgroup_id`       | `u64`   | サブグループ ID                     |
| `publisher_priority`| `u8`    | Publisher の優先度                  |
| `payload`           | `bytes` | エンコード済みフレームデータ         |

### Redis CLI での確認方法

```bash
# チャネルのメッセージを購読 (デバッグ用)
redis-cli SUBSCRIBE moq:track:live:video

# アクティブなチャネル一覧
redis-cli PUBSUB CHANNELS "moq:track:*"
```

## 技術詳細

- **トランスポート**: [wtransport](https://crates.io/crates/wtransport) v0.7 (WebTransport over HTTP/3)
- **中継**: Redis Pub/Sub (MessagePack シリアライズ)
- **映像コーデック**: H.264 (ブラウザ側は WebCodecs API)
- **音声コーデック**: Opus (ブラウザ側は WebCodecs AudioEncoder/AudioDecoder)
- **プロトコルバージョン**: `0xff000001` (draft)

## ライセンス

MIT
