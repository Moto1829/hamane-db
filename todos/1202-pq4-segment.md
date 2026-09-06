# 1202: 4-bit PQ のセグメント統合と StoreOptions

- Status: DONE (2026-09-06)
- Milestone: M12
- Depends: 1201
- Design: docs/design/quantization.md §8

## ゴール

vectors_pq.bin / ivfpq.bin を nbits=4 で書けるようにする。ヘッダに既に
nbits/ksub があるので**フォーマットの拡張は不要** (自己記述的)。

## やること

- [x] `StoreOptions.pq_nbits: u8` (既定 8) を追加。validate で 4/8 のみ許可、
      quantization=Pq 以外との併用を拒否
- [x] `IndexBuildSpec.pq_nbits` を通し、write_pq / write_ivfpq が使う
- [x] `load_pq` / `load_ivfpq`: nbits 4/8 を受け入れ、コード領域サイズを
      `count × code_bytes(m, nbits)` で検証
- [x] `PqView::code` / `IvfPqView::code_distance` を code_bytes 基準に

## 完了条件

- [x] nbits=4 でフラッシュ → 再 open → 検索が通る (recall は再ランクで担保)
- [x] vectors_pq.bin のサイズが nbits=8 の約半分
- [x] nbits=8 の既存セグメントがそのまま読める (既定は 8 のまま)

## 実装メモ

- ヘッダに nbits/ksub が最初から入っていたのでフォーマット変更はゼロ。
  ロード側の検証を `nbits ∈ {4,8}` と `count × code_bytes(m, nbits)` に緩めただけ
- **4bit は 1 段目が粗いので再ランク候補を k×4 → k×8 に増やした**
  (`rerank_factor(nbits)`)。dim=16 乱数で m=4,4bit が 0.685 → 0.822、
  同バイト数の m=8,4bit が 0.952 → 1.000 に改善
- `StoreOptions.pq_nbits` は 4/8 のみ、かつ PQ 専用 (validate で拒否、
  invalid_options_rejected に 2 ケース追加)
