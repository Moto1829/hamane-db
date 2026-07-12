# 204: セグメント書き出し

- Status: DONE (2026-07-12)
- Milestone: M2
- Depends: 201, 203
- Design: docs/design/storage.md §3

## ゴール

memtable の内容を不変セグメント (vectors/ids/meta/tombstones) として
原子的にディスクへ書き出す。

## やること

- [ ] `SegmentWriter::write(dir, seg_id, &MemtableSnapshot) -> SegmentMeta`
  - `seg-<id>.tmp/` に 4 ファイルを書く → 各ファイル fsync →
    `seg-<id>/` へ rename → 親ディレクトリ fsync
- [ ] vectors.bin: 64B pad ヘッダ + 行指向 f32 + CRC (storage.md §3)
- [ ] ids.bin: 行順 id 列 + (id,row) 昇順索引 + CRC
- [ ] meta.bin: offsets + blob 連結 + CRC
- [ ] tombstones.bin: 昇順 id 列 + CRC
- [ ] 行順は id 昇順で決定的にする (テスト・デバッグのしやすさのため)

## 完了条件

- 書き出したファイルのバイトレイアウトを検証するユニットテスト
  (ヘッダ・アラインメント・CRC)
- 同じ memtable から 2 回書くと同一バイト列になる (決定性)
