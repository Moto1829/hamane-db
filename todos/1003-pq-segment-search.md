# 1003: PQ セグメント統合と ADC 2 段階検索

- Status: DONE (2026-09-01)
- Milestone: M10
- Depends: 1002
- Design: docs/design/quantization.md §1.3〜1.4, §5

## ゴール

セグメントに vectors_pq.bin を書き、HNSW を ADC 距離で辿って f32 再ランクする
2 段階検索を通す (SQ8 と同じ枠組み・同じ RERANK)。

## やること

- [x] `SegmentWriter` の spec に PQ を追加。フラッシュ/コンパクション時に
      全 f32 行から `PqCodebook::train` (seed=seg_id) → 各行 encode →
      vectors_pq.bin 書き出し (magic `HAMANEP`、header 64B + codebook f32 +
      codes u8 + crc32c)
- [x] `PqView` (借用ビュー): `build_lut` / `distance_key(lut, row)`。
      コードブックは `Segment::open` 時に一度だけ復元 (`load_pq` が全整合検証)、
      コード列は mmap のまま zero-copy
- [x] `StoreOptions.quantization: Option<Quantization>` を追加し `sq8: bool` を
      `Quantization::{Sq8, Pq{m}}` に統合 (既定 None)
- [x] `collection.rs` の検索: HNSW あり・フィルタなし経路で PqView があれば
      ADC で `k × RERANK` 探索 → vectors.bin f32 で再ランク → top-k。
      SQ8 と共通の `two_stage_search` ヘルパに集約
- [x] m 未指定かつ dim から自動決定不可 (素数次元) の場合は PQ をスキップし
      通常 HNSW にフォールバック (フラッシュを失敗させない)

## 完了条件

- [x] PQ on で recall@10 ≥ 0.95 を維持 (統合テスト
      `pq_two_stage_search_preserves_recall`、再 open 後も PQ 経路が生きる)
- [x] ベクトル部のディスクが m バイト/行 + codebook に縮小
      (dim=768/m=96 で codes は ~32x 圧縮)
- [x] off / SQ8 の既存動作・フォーマットに影響なし (全クレート green、clippy 0)

## 実装メモ

- magic 競合を発見: SQ8 が既に `HAMANEQ` を使用。PQ は `HAMANEP`、
  設計文書の IVF-PQ は `HAMANEQ`→`HAMANEG` に修正済み
- `sq8: bool` を廃止し `quantization: Option<Quantization>` に一本化
  (影響は store.rs と hnsw_integration テストのみ。他クレートは未使用)
- コードブック (~MB) は open 時に owned 復元し検索ごとの再構築を回避。
  巨大な codes 列は mmap 参照のまま (SQ8 と同じ zero-copy 方針)
