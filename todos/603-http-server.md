# 603: hamane-server (HTTP API)

- Status: DONE (2026-07-14)
- Milestone: M6
- Depends: 504 (サーバ用途では書き込みストール解消が前提)
- Design: DESIGN.md §7 (サーバ層は将来拡張として設計済み)

## ゴール

組み込みライブラリの上に薄い HTTP サーバを載せ、他言語・他プロセスから
使えるようにする。

## やること

- [ ] `crates/hamane-server` (axum + tokio)。Store は Send+Sync 済みなので
      Arc<Database> を共有するだけ。ブロッキング呼び出しは spawn_blocking で
- [ ] エンドポイント (JSON):
  - `PUT /collections/{name}` (dim, metric)
  - `DELETE /collections/{name}` / `GET /collections`
  - `POST /collections/{name}/records` (単発・バッチ upsert)
  - `DELETE /collections/{name}/records/{id}`
  - `POST /collections/{name}/search` (vector, k, ef, filter — CLI と同じ
    フィルタ JSON 表現を共有クレート化して流用)
  - `POST /admin/flush` / `POST /admin/compact`
- [ ] エラーの HTTP ステータス対応 (DimensionMismatch → 400 等)
- [ ] グレースフルシャットダウン (flush してから終了)
- [ ] 結合テスト: サーバを起動して reqwest で一連の CRUD + 検索

## 完了条件

- 上記エンドポイントの結合テスト green
- README にサーバの起動・curl 例を追記
- 認証・TLS はスコープ外と明記 (リバースプロキシ前提)
