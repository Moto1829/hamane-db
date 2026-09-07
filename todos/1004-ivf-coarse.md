# 1004: IVF 粗量子化と nprobe 検索

- Status: DONE (2026-09-01)
- Milestone: M10
- Depends: 1002
- Design: docs/design/quantization.md §2, §4, §5

## ゴール

セグメントに ivf.bin (粗セントロイド + CSR 転置リスト) を書き、nprobe クラスタ
だけを Flat スキャンする枝刈り検索を通す。HNSW とはセグメント単位で排他。

## やること

- [x] `StoreOptions.index: IndexKind { Hnsw, Ivf }` (既定 Hnsw) を追加。
      Ivf のセグメントは hnsw.bin を書かず ivf.bin を書く (IvfPq は 1005)
- [x] 粗 k-means: `nlist = clamp(round(sqrt(count)), 16, 65536)`、
      count < nlist×39 なら nlist を縮小 (`choose_nlist`)。1002 の kmeans を
      dim 次元で再利用、割り当ては `pq::nearest`、学習は `pq::subsample`
- [x] ivf.bin 書き出し (magic `HAMANEC`、header 64B + centroids f32 +
      CSR offsets(u64)/entries(u32) + crc32c)。`IvfView::candidate_rows(query,
      nprobe)` が nprobe リストの全行番号を返す (L2 で粗量子化選択)
- [x] `collection.rs`: IVF 経路を追加 (hnsw() チェックより前)。候補行を
      live/フィルタ適用の Flat スキャン → top-k。memtable は従来 Flat
- [x] `nprobe` を検索時に上書き可能に (`SearchBuilder::nprobe`、ef と同様)。
      既定は `StoreOptions.nprobe`

## 完了条件

- [x] nprobe を上げると recall が改善 (nprobe=8 ≥ nprobe=1)、full-probe で
      Flat 正解に一致 (recall ≥ 0.999)。統合テスト `ivf_nprobe_improves_recall`
- [x] 走査行数が nprobe/nlist に減る (candidate_rows が選択リストのみ返す)
- [x] Ivf 未指定時は一切変化なし (全クレート green、clippy 0)

## 実装メモ

- 粗セントロイドは `Segment::open` 時に owned 復元 (`load_ivf` が CSR 末尾
  オフセット = count まで検証)、offsets/entries は mmap zero-copy (PQ と同方針)
- HNSW と IVF はセグメント単位で排他。`SegmentWriter` を `write_hnsw` /
  `write_ivf` に分割し `IndexKind` で振り分け
- IVF と量子化の併用は StoreOptions.validate で禁止 (IVF-PQ は 1005)
- 粗量子化選択は k-means と同じ L2 固定 (metric 非依存)。Cosine は正規化済み
  なので L2 最近傍 = 角度最近傍
