# 206: manifest と CURRENT の原子的切り替え

- Status: DONE (2026-07-12)
- Milestone: M2
- Depends: 201
- Design: docs/design/storage.md §4

## ゴール

有効なセグメント構成の世代管理を実装し、どの時点でクラッシュしても
完全な世代に復帰できるようにする。

## やること

- [ ] `Manifest` 構造体 (gen, next_collection_id, next_seg_id, wal_seq,
      collections[…segments]) と encode/decode (CRC つき)
- [ ] `Manifest::store(db_dir)`:
  1. `MANIFEST-<gen+1>` 書き込み + fsync
  2. `CURRENT.tmp` → fsync → `rename(CURRENT)` → 親ディレクトリ fsync
- [ ] `Manifest::load(db_dir)`: CURRENT → manifest 読み込み + CRC 検証
- [ ] 掃除: CURRENT が指さない MANIFEST / `.tmp` 残骸の削除 (`gc()`)

## 完了条件

- store → load のラウンドトリップ
- store の各ステップ間で「クラッシュした状態」(途中ファイルを手で作る) から
  load すると必ず旧世代または新世代のどちらか完全な方が読めるテスト
