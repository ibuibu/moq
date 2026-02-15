# CLAUDE.md

## プロジェクト概要

MoQ (Media over QUIC Transport) の最小限実装。Rust + WebTransport でリアルタイム映像・音声を中継するサーバーと、CLI/ブラウザクライアントを提供する。

## ビルド・テスト

```bash
# ビルド
cargo build

# テスト
cargo test

# サーバーのみビルド
cargo build --bin moq-server

# クライアントのみビルド
cargo build --bin moq-client
```

## コード構成

- `src/varint.rs` — QUIC 可変長整数 (RFC 9000 Section 16)
- `src/message.rs` — MoQ プロトコルメッセージの encode/decode
- `src/session.rs` — SETUP ハンドシェイクとメッセージ送受信ヘルパー
- `src/publisher.rs` — Publisher (Object 送信ロジック)
- `src/subscriber.rs` — Subscriber (Object 受信ロジック)
- `src/bin/moq-server.rs` — サーバー本体 (broadcast channel で中継 + `/config` API)
- `src/bin/moq-client.rs` — CLI クライアント (publish/subscribe/video-publish/video-subscribe)
- `frontend/` — ブラウザクライアント (Vite で開発・ビルド)

## 依存クレート

- `wtransport` 0.7 — WebTransport (HTTP/3 over QUIC)。`dangerous-configuration` feature で証明書検証スキップを有効化
- `tokio` — 非同期ランタイム
- `bytes` — バッファ操作
- `image` + `sdl2` — CLI video モードで使用

## ブラウザクライアント開発

```bash
# Vite dev server 起動 (moq-server と併用)
cd frontend && npm install && npm run dev
```

- Vite dev server: `http://localhost:5173` (Viewer) / `http://localhost:5173/publisher.html` (Publisher)
- `/config` API は Vite のプロキシ経由で `http://localhost:8080/config` に転送される
- ブラウザクライアントは Chrome 限定 (WebTransport + WebCodecs が必要)

## 開発上の注意点

- wtransport の stream API: `open_bi().await?.await?` (2段階await)、`accept_bi().await?` (1段階)
- moq-server は WebTransport + `/config` API のみ。HTML/JS の配信は行わない
- ブラウザクライアントは `frontend/` ディレクトリで Vite を使って開発・ビルドする
