# 304: hnsw.bin 直列化と mmap ロード

- Status: DONE (2026-07-12)
- Milestone: M3
- Depends: 302 (フォーマットは 201 の基盤を使用)
- Design: docs/design/index.md §3

## ゴール

構築済みグラフをセグメントファイルとして永続化し、デシリアライズなしの
mmap ビューで探索できるようにする。

## やること

- [ ] `HnswBuilder::serialize(writer)`: index.md §3 の CSR レイアウト
      (header 64B pad / levels / 層ごとの node_ids + offsets + neighbor_ids / CRC)
- [ ] `HnswView::open(mmap 領域)`: ヘッダ検証 + 各配列へのオフセット解決のみ
      (コピーなし。u32 アラインメント検証)
- [ ] `HnswView` に `HnswGraph` trait (302) を実装
  - 層内の `neighbors(level, node)`: node_ids の二分探索 → offsets → slice
- [ ] hamane-storage の `Segment` に hnsw.bin の有無を扱うフィールド追加は
      305 で行う (このタスクはフォーマットとビューまで)

## 完了条件

- serialize → open で `HnswGraph` としての観測 (levels / neighbors / entry_point)
  がビルダーと完全一致
- ビルダー探索と HnswView 探索が同一クエリで同一結果
- 破損 (CRC / アラインメント / count 不整合) が Corrupted になる
