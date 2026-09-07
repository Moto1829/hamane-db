# 1103: opq.bin のセグメント統合とクエリ回転

- Status: DONE (2026-09-06)
- Milestone: M11
- Depends: 1102
- Design: docs/design/opq.md §2〜§4

## ゴール

セグメントに opq.bin を書き、PQ / IVF-PQ の符号化と探索を回転後空間で行う。
opq.bin が無ければ従来 PQ (M10 セグメントの前方互換)。

## やること

- [x] `StoreOptions.opq: bool` (既定 false) を追加。`validate` で
      quantization = Pq 以外との併用を拒否
- [x] `format.rs`: `MAGIC_OPQ = b"HAMANEO\x01"`、`segment.rs`: `FILE_OPQ`
- [x] `write_pq` / `write_ivfpq` の入口で回転を学習し、回転済みデータで
      以降の処理 (コードブック学習・粗 k-means・残差) を実行
- [x] opq.bin 書き出し (header 64B + dim×dim f32 + crc32c)、`load_opq` で
      dim 一致と直交性を検証。`Segment::opq_view()` / `has_opq()`
- [x] `collection.rs`: PQ / IVF-PQ 経路で LUT 構築前にクエリを回転する
      (再ランクは生 f32 のまま = 変更なし)

## 完了条件

- [x] `Hnsw+Pq+opq` / `IvfPq+opq` で recall@10 ≥ 0.95 (統合テスト)
- [x] opq.bin の無いセグメント (M10 相当) が従来どおり読める
- [x] opq=false で全既存テストが green、clippy 0

## 実装メモ

- 書き出しは `SegmentWriter::train_opq` に集約 (回転学習 → opq.bin 書き出し →
  回転後ベクトルを返す)。`write_hnsw` の PQ 分岐と `write_ivfpq` の両方が
  これを呼び、以降は**回転後データを使うだけ**で M10 の処理を変えていない
- IVF-PQ では粗量子化も残差 PQ も回転後空間で行う。回転は L2 を保存するので
  粗セントロイドの意味は不変
- 検索側の差分は `collection.rs` の 2 箇所のみ (PQ 経路の `build_lut` 前と
  IVF-PQ 経路の `nprobe_lists` / `build_list_lut` 前でクエリを回す)。
  再ランクは生 f32 のままなのでスコアは回転の影響を受けない
- opq.bin の CRC は他ファイルと同じく `verify_checksums` の担当 (open 時は
  検証しない = 高速起動)。代わりに **open 時に直交性 `‖RᵀR − I‖ < 1e-3` を
  検証**するので、意味のある破損はここで弾ける。CRC 検証のために
  `OpqSegment { rotation, mapped }` として mmap を保持する
- `StoreOptions.opq: bool` は `Quantization::Pq` 以外との併用を validate で拒否
  (invalid_options_rejected に 2 ケース追加)
- 統合テスト 4 本: PQ+OPQ / IVF-PQ+OPQ の recall、opq.bin 削除後も open 可
  (前方互換)、破損 opq.bin の拒否。全 159 テスト green、clippy 0 (1.98)
