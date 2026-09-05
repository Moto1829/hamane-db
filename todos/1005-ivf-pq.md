# 1005: IVF-PQ (residual PQ) 統合

- Status: DONE (2026-09-05)
- Milestone: M10
- Depends: 1003, 1004
- Design: docs/design/quantization.md §3, §5

## ゴール

IVF の枝刈りと PQ の圧縮を組み合わせ、残差ベクトルを PQ 符号化した ivfpq.bin で
検索する。生 PQ より低誤差で、かつ全走査を避ける。

## やること

- [x] 粗 k-means (1004) で nlist セントロイドと各行の所属リストを得る
- [x] 全行の残差 `x − centroid[list]` を集めて PQ コードブックを 1 組学習
      (リスト非依存の共有コードブック、1002 を残差空間で使う)
- [x] ivfpq.bin 書き出し (magic `b"HAMANEQ\x01"`、coarse centroids + pq codebook
      + CSR offsets/entries/codes + crc32c)。`IvfPqView`: probe 選択 →
      probe ごとに残差 LUT 構築 → 表引き
- [x] `collection.rs`: IvfPq 経路。nprobe リストを残差 ADC で `k × PQ_RERANK`
      候補化 → マージ → vectors.bin f32 で再ランク → top-k
- [x] 許可する index × quantization を 5 通りに制限し、他は open 時エラー

## 完了条件

- [x] IVF-PQ on で高 recall (再ランクあり)。統合テスト
      `ivfpq_two_stage_search_preserves_recall` (一様乱数 3000 件、full-probe で
      recall@10 ≥ 0.90)、SIFT 200k 実測は 0.9852 (nprobe=32、docs/benchmarks.md)
- [x] 同 m の生 PQ より残差版が低い量子化誤差 (ユニットテスト
      `pq::tests::residual_pq_has_lower_error_than_raw_pq` で再構成 MSE を比較)
- [x] メモリ = PQ 相当 (コード m バイト/行) かつ走査は nprobe/nlist に枝刈り
- [x] off / 他構成の既存動作に影響なし (全テスト green、clippy 0)

## 実装メモ

- 粗量子化選択は 1004 と同じ L2 固定。残差 LUT は metric で場合分けする:
  L2 は「クエリ残差 `q − centroid`」で LUT を張り bias=0、Dot/Cosine は
  `dot(q, x) = dot(q, centroid) + dot(q, r)` なので LUT は生クエリのまま
  bias = `−dot(q, centroid[l])` (`IvfPqView::build_list_lut`)
- CSR は offsets/entries に **codes を同順で並べた 3 本組**。probe したリストの
  コードだけが連続領域に載るのでキャッシュ効率がよい。粗セントロイドと
  コードブックのみ open 時に owned 復元、CSR は mmap zero-copy (1003/1004 と同方針)
- m が dim を割り切れない・自動決定できない場合は通常 IVF にフォールバック
  (`write_ivfpq` 冒頭)。ivfpq.bin が無ければ従来経路
- IvfPq は `Quantization::Pq` 必須、Ivf は量子化不可 —
  `StoreOptions::validate` で open 時に弾く (許可は 5 通り)
