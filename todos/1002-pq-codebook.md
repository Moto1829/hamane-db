# 1002: PQ コードブック学習 (k-means)

- Status: DONE (2026-09-01)
- Milestone: M10
- Depends: 1001
- Design: docs/design/quantization.md §1.1〜1.2, §4

## ゴール

hamane-core に PQ のコードブック学習・符号化・ADC LUT の純粋な数値処理を実装する
(ストレージ非依存。sq8.rs と対称に pq.rs へ)。

## やること

- [x] `hamane-core/src/pq.rs`: k-means (Lloyd + k-means++ 初期化、seed 固定、
      空クラスタ再割り当て)。粗量子化 (§2) と共有できる汎用 `kmeans()` 関数
- [x] `PqCodebook { m, dsub, centroids: Vec<f32> }` (nbits=8 = KSUB 256 固定):
  - `train(vectors, dim, m, seed, train_sample, max_iter) -> Result<Self>`
        (件数が train_sample 超なら等間隔ストライドで間引く)
  - `encode(vector, out)` (各サブベクトルの L2 最近傍セントロイド ID)
  - `build_lut(query, metric) -> Vec<f32>` (m × KSUB の距離表。「小さいほど
        近い」に正規化済み = L2 は距離²、Dot/Cosine は内積の符号反転)
  - `distance_key(lut, code) -> f32` (m 回の表引き加算)
  - `from_centroids` / `centroids()` (1003 の直列化・mmap ロード用)
- [x] nbits=8 のみ実装。`dim % m != 0` は `InvalidConfig`
- [x] `choose_m(dim)` 自動決定 (dim の約数で dsub≥4 かつ 2≤m≤96 の最大 m。
      m=1 を避けるため dsub 探索は dim/2 まで。素数次元は None)

## 完了条件

- [x] ADC 推定距離と f32 実距離の相対誤差がクラスタデータで 15% 以内、
      k×4 候補で真の top-10 の recall ≥ 0.9 (決定的 seed のユニットテスト)
- [x] k-means が空クラスタ・重複データ・点数<k で panic せず、学習が決定的
- [x] L2 / dot 両メトリックで LUT と実距離の大小関係が一致

## 実装メモ

- 外部 rand を増やさず決定的 LCG を pq.rs に内蔵 (sq8.rs と同方針)。
  距離計算は全て `l2_squared_scalar` / `dot_scalar` でプラットフォーム非依存
  (同一マシンでの再構築が決定的)
- `build_lut` が符号を吸収するので `distance_key` は metric 非依存の表引き加算
- k-means は IVF (1004) の粗量子化と共有する汎用形 (`points: &[&[f32]]`)。
  IVF は dim 次元で、PQ は dsub 次元サブベクトルで同じ関数を呼ぶ
- 全 24 テスト green、clippy 0
