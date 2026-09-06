# 1102: OPQ 回転行列の学習 (hamane-core/opq.rs)

- Status: DONE (2026-09-06)
- Milestone: M11
- Depends: 1101
- Design: docs/design/opq.md §1, §5

## ゴール

hamane-core に直交回転の学習・適用を実装する (ストレージ非依存。pq.rs と対称に
opq.rs へ)。SVD を使わず、極分解の Newton 反復と Gauss-Jordan 逆行列だけで解く。

## やること

- [x] `hamane-core/src/opq.rs`: 行列ユーティリティ (積 `matmul`、転置、
      Gauss-Jordan 逆行列 `invert`)。全て row-major の `Vec<f32>`、d×d
- [x] `polar(m, d) -> Option<Vec<f32>>`: Newton 反復 `Q ← ½(Q + Q⁻ᵀ)` で
      直交極因子を求める。逆行列が求まらなければリッジを足して再試行、
      それでも失敗したら `None`
- [x] `OpqRotation { dim, r: Vec<f32> }`:
  - `train(vectors, dim, m, seed, ...) -> Result<Self>` (交互最適化 T 回)
  - `apply(x, out)` / `apply_vec(x) -> Vec<f32>` (x' = R x)
  - `from_matrix(dim, r) -> Result<Self>` (mmap ロード用、直交性を検証)
  - `matrix() -> &[f32]` (直列化用)、`identity(dim)`
- [x] 定数 `OPQ_ITERS = 8`, `OPQ_TRAIN_SAMPLE = 16384`, `OPQ_INNER_ITER = 5`
- [x] 縮退時のフォールバック: 極分解失敗・直交性検証失敗・m 不正なら
      恒等行列を返して学習を絶対に失敗させない

## 完了条件

- [x] 学習した R が直交 (`‖RᵀR − I‖_max < 1e-3`)
- [x] 分散が偏り相関のある人工データで、同じ m・同じ seed なら
      **OPQ の再構成 MSE < PQ の再構成 MSE**
- [x] 全点同一・点数 < ksub・特異行列で panic せず恒等行列にフォールバック
- [x] 学習が決定的 (同じ seed で同じ R)

## 実装メモ

- **初期値は恒等と乱数の両方を試し、学習サンプル上の実測誤差で選ぶ**。
  片方に決め打つと必ずどちらかのデータで悪化する:
  恒等は軸に沿った独立分布で停留点になり動かず、乱数は構造化データ
  (SIFT のようにサブベクトルが元から相関) の出発点として悪すぎる。
  さらに「回転なし」とも比較し、勝てなければ恒等を返す
  (**OPQ で素の PQ より悪くならない**ことを保証)
- **極分解には Higham のスケーリングが必須**。`M = ŶᵀX` は成分が 1e3〜1e4 に
  なり、素の Newton 反復では 50 回でも直交に収束せず「極分解失敗 → 学習が
  1 歩も進まない」になる。実際にこれで OPQ が乱数回転のままになり SIFT の
  recall が PQ より悪化した (ベンチで検出、`polar_converges_on_large_scale_matrix`
  で回帰防止)
- 逆行列は桁落ちを避けるため f64 で Gauss-Jordan (部分ピボット選択)。
  取れないたびにリッジを 10 倍しながら最大 8 回粘る
- `PqCodebook::decode_into` を追加 (コード → ベクトル復元)。交互最適化と
  誤差計測の両方で使う。pq.rs の `Lcg` は `pub(crate)` に変更して共有
- 誤差比較テストは**次元ごとの分散が等比で偏ったフルランクのガウス**を使う。
  低ランクデータ (潜在変数が次元より少ない) だと `M` が特異で極分解が
  収束せず、テストの意図 (回転の効果) が測れない
- opq 11 テスト + core 全 36 テスト green、clippy 0 (1.98 でも確認)
