# 305: フラッシュ統合とマージ検索

- Status: DONE (2026-07-12)
- Milestone: M3
- Depends: 303, 304, 209
- Design: docs/design/index.md §4, docs/design/query.md §2

## ゴール

セグメントフラッシュ時に HNSW を構築・永続化し、検索パスを
「memtable=Flat + セグメント=HNSW」のマージに切り替える。

## やること

- [ ] `SegmentWriter`: `record_count >= hnsw_min_rows` (既定 1024) なら
      構築して hnsw.bin を書く。seed は seg_id (決定的構築)
- [ ] `Segment::open`: hnsw.bin があれば `HnswView` を保持
- [ ] `run_search` のソース別プラン (query.md §2 の 3):
      hnsw あり → HNSW(ef) / なし → Flat
- [ ] `SearchBuilder::ef(usize)` を公開 API に追加 (既定は params の ef_search)
- [ ] `CollectionConfig.hnsw: HnswParams` を追加 (Default 維持)
- [ ] 再現率の統合テスト: flush を挟んだ Collection 全体で recall@10 ≥ 0.95
      (memtable 分は正確なので、セグメント分のみの検証にもなる)

## 完了条件

- 上記統合テスト green + 既存の全テスト green
- hnsw.bin なしセグメント (小規模) と混在しても正しく動く
- DESIGN.md M3 の機能面 (構築・永続化・マージ検索) が完了
  (recall ゲートの実測は 307)
