# moq

Media over QUIC Transport (MoQ) の最小限実装。WebTransport 上でリアルタイム映像・音声をやり取りするクライアント・サーバーを Rust で実装している。

## 概要

- **moq-server** — WebTransport サーバー + `/config` API。Publisher からの映像/音声を受け取り、Subscriber に中継する
- **moq-client** — CLI クライアント。テキスト/映像の publish/subscribe をサポート
- **ブラウザクライアント** — WebTransport API を使ったブラウザベースの Publisher/Viewer

## アーキテクチャ

```
Browser Publisher ──┐                  ┌── Browser Viewer
                    ├─ WebTransport ─► moq-server ─┤
CLI Publisher ──────┘   (QUIC/UDP)     └── CLI Subscriber
```

- 制御メッセージ: Bidirectional Stream で送受信 (ClientSetup, ServerSetup, Publish/Subscribe, etc.)
- メディアデータ: Unidirectional Stream で Object 単位に送信
- サーバーは `tokio::sync::broadcast` で Publisher → Subscriber にデータを中継

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
    moq-server.rs    # サーバー (中継 + /config API)
    moq-client.rs    # CLI クライアント (publish/subscribe/video-publish/video-subscribe)
frontend/
  index.html         # ブラウザ Viewer (H.264 + Opus デコード)
  publisher.html     # ブラウザ Publisher (H.264 + Opus エンコード)
  common.js          # 共有ユーティリティ (VarInt, String encode/decode, fetchConfig, etc.)
  viewer.js          # Viewer 固有ロジック
  publisher.js       # Publisher 固有ロジック
  package.json       # Vite 開発環境
  vite.config.js     # Vite 設定 (/config プロキシ)
```

## セットアップ

### 必須

- Rust (edition 2024)
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

### サーバー起動

```bash
cargo run --bin moq-server
```

以下が起動する:
- WebTransport サーバー: `:4433`
- Config API: `http://localhost:8080/config`

### ブラウザクライアント

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

## 技術詳細

- **トランスポート**: [wtransport](https://crates.io/crates/wtransport) v0.7 (WebTransport over HTTP/3)
- **映像コーデック**: H.264 (ブラウザ側は WebCodecs API)
- **音声コーデック**: Opus (ブラウザ側は WebCodecs AudioEncoder/AudioDecoder)
- **プロトコルバージョン**: `0xff000001` (draft)

## ライセンス

MIT
