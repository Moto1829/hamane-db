# 902: primary 側 /replication API

- Status: DONE (2026-07-18)
- Milestone: M9
- Depends: 901
- Design: docs/design/replication.md §2

## ゴール

hamane-server に replica が同期に使う 4 エンドポイントを追加する。
ストレージエンジンは変更しない (db_dir のファイルを読むだけ)。

## やること

- [x] GET /replication/state (manifest_gen / manifest_name / wal_seq / wal_len)
- [x] GET /replication/manifest/{gen}
- [x] GET /replication/segment/{seg_id}/{file} (ファイル名はホワイトリスト検証)
- [x] GET /replication/wal/{seq}?offset=N (append-only の tail 読み)
- [x] すべて Content-Length 付き単純ボディ・既存 API キー認証の内側
- [x] 結合テスト: state の整合、offset 読み、存在しない gen/seg の 404

## 完了条件 — 達成 (結合テスト 4 本 green)

- flush / compact を挟んでも state とファイル取得が一貫する
- パストラバーサル不可 (`file` はセグメント構成ファイル名のみ許可)
