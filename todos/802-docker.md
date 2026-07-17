# 802: Docker イメージ

- Status: DONE (2026-07-18)
- Milestone: M8
- Depends: 603, 705
- Design: crates/hamane-server (薄いラッパ方針のまま配布手段を足す)

## ゴール

Rust ツールチェーンなしで `docker run` 一発で hamane-server を立ち上げ
られるようにする。静的リンクの最小イメージ (scratch ベース) で「軽さ」を
そのまま配布形態にする。

## やること

- [x] `/health` エンドポイント (認証不要。orchestrator の probe 用)
- [x] SIGTERM でのグレースフルシャットダウン (現状 Ctrl-C のみ。
      Docker の stop は SIGTERM)
- [x] `--db` / `--listen` の環境変数対応 (HAMANE_DB / HAMANE_LISTEN)
- [x] `--healthcheck` フラグ: 自分の /health を std::net で叩いて exit 0/1
      (scratch には curl がないため HEALTHCHECK 用)
- [x] Dockerfile: rust:alpine builder (musl 静的リンク) → scratch、
      非 root (65534)、HEALTHCHECK、/data ボリューム
- [x] .dockerignore (target / data / .venv を除外)
- [x] docker-compose.yml (ビルド + 永続ボリュームの最小例)
- [x] CI: tag push (v*) で GHCR へ multi-arch (amd64/arm64) publish
- [x] README とドキュメントに Docker 節を追記

## 完了条件 — すべて達成

- `docker build` が通り、`docker run` したコンテナに対して
  create → upsert → search → 再起動後も検索できる (ボリューム永続化)
- `docker stop` (SIGTERM) で flush してから終了する
- イメージサイズが二桁 MB 以下

## 実装メモ

- イメージサイズ **2.63MB** (musl 静的リンク + strip + scratch)
- スモークテスト済み: health / create / upsert / search / 認証なし 401 /
  `docker stop` で flush ログ / ボリューム再マウントで検索結果維持 /
  `--healthcheck` exit 0
- GHCR への publish は tag `v*` push か workflow_dispatch (edge タグ)。
  main への Dockerfile 変更はビルド検証のみ
