# moq

Media over QUIC Transport (MoQ) の最小限実装。WebTransport 上でリアルタイム映像・音声をやり取りするクライアント・サーバーを Rust で実装している。

## 概要

- **moq-server** — WebTransport サーバー + HTTP 配信サーバー。Publisher からの映像/音声を受け取り、Subscriber に中継する
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
  error.rs           # エラー型
  session.rs         # SETUP ハンドシェイク・メッセージ送受信ヘルパー
  publisher.rs       # Publisher (Object 送信)
  subscriber.rs      # Subscriber (Object 受信)
  bin/
    moq-server.rs    # サーバー (中継 + HTTP 配信)
    moq-client.rs    # CLI クライアント (publish/subscribe/video-publish/video-subscribe)
static/
  viewer.html        # ブラウザ Viewer (H.264 + Opus デコード)
  publisher.html     # ブラウザ Publisher (H.264 + Opus エンコード)
  common.js          # 共有ユーティリティ (VarInt, String encode/decode, etc.)
  viewer.js          # Viewer 固有ロジック
  publisher.js       # Publisher 固有ロジック
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
- HTTP サーバー: `http://localhost:8080`

### ブラウザクライアント

サーバー起動後、Chrome でアクセス:

- Viewer: http://localhost:8080
- Publisher: http://localhost:8080/publish

自己署名証明書のハッシュと接続先 IP は HTML に自動埋め込みされる。

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
