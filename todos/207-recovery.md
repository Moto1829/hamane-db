# 207: Database::open と復旧

- Status: DONE (2026-07-12)
- Milestone: M2
- Depends: 202, 205, 206
- Design: docs/design/storage.md §5, docs/design/query.md §3

## ゴール

`Database::open(path)` を実装し、再起動後に manifest + WAL リプレイで
直前の状態を完全復元する。

## やること

- [ ] `Store` (hamane-storage): manifest / segments / WAL を束ねる装置
  - `Store::open(db_dir)`: storage.md §5 の手順 1〜5
  - `Store::in_memory()`: WAL/セグメントを持たない空実装 (query.md §3 の方針)
- [ ] `Database::open(path)` を公開。`Database::in_memory()` は Store::in_memory に委譲
- [ ] create/drop_collection を WAL 経由に変更 (WalRecord::CreateCollection 等)
- [ ] upsert/delete を WAL append + sync → memtable の順に変更 (query.md §1)
- [ ] リプレイ後の WAL 切り詰め (WalReader が返した停止位置で truncate)

## 完了条件

- upsert → drop せず再 open → 全レコードが get / search で見える統合テスト
- create_collection だけして再 open → collection が存在する
- 未知ディレクトリ / 空ディレクトリ / 既存 DB の 3 パターンで open が正しく動く
