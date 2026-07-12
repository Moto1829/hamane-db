# 203: Memtable の分離 (tombstone 対応)

- Status: DONE (2026-07-12)
- Milestone: M2
- Depends: 201
- Design: docs/design/storage.md §6

## ゴール

現在 `hamane::Collection` 内の `HashMap<Id, StoredRecord>` を hamane-storage の
`Memtable` に移し、削除マーカーとサイズ会計を持たせる。

## やること

- [ ] `Memtable { upserts, deletes, bytes }` (storage.md §6 の仕様どおり)
  - upsert は deletes を打ち消す / delete は upserts を打ち消して marker を残す
  - `bytes`: ベクトル + メタデータの概算 (dim×4 + キー・値長の合計)
- [ ] 検索用イテレータ `iter() -> (Id, &[f32], &Metadata)` (search_flat に渡せる形)
- [ ] スナップショット取得 `snapshot() -> MemtableSnapshot` (v0 は clone)
- [ ] `hamane::Collection` の内部を Memtable に置き換え (公開 API 不変、
      deletes はこの時点では検索に影響しない = 全データが memtable にあるため)

## 完了条件

- 既存の hamane 統合テスト (tests/api.rs) が無修正で green
- upsert→delete→upsert の系列で marker が正しく遷移するユニットテスト
