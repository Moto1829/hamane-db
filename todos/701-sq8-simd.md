# 701: SQ8 の u8 SIMD カーネル

- Status: DONE (NEON 2026-07-15 / AVX2 2026-09-11)
- Milestone: M7
- Depends: 602
- Design: crates/hamane-core/src/sq8.rs (602 の残課題)

## ゴール

SQ8 の距離計算 (sq8_l2_accum / sq8_dot_accum) を NEON / AVX2 で高速化し、
SQ8 経路の検索スループットを f32 経路より速くする。

## やること

- [x] NEON (aarch64): vabd (絶対差) + vmull_u8 + vpadal の widening 累積
- [x] AVX2 (x86_64): 実行時ディスパッチは f32 カーネルと同じ方式。
      ただし **maddubs は使えない** (第 2 オペランドが符号付きなので、差が
      128 以上のときに負と解釈される)。16 ビットへ展開して `madd_epi16` で積和する
- [x] u32 アキュムレータのオーバーフロー境界 (dim ≤ 66051) を doc に明記
- [x] スカラー実装との完全一致テスト (整数演算なので誤差ゼロで比較)
- [x] criterion ベンチ (distance.rs に追加) で f32 SIMD と比較

## 完了条件

- 全プラットフォームでスカラー一致テスト green
- dim=768 で SQ8 SIMD が f32 SIMD (NEON) の 2 倍以上のスループット
- docs/benchmarks.md に記録

## 実装メモ (AVX2、2026-09-11)

- `maddubs` が使えないのが肝。`|a−b|` は飽和減算の両方向 OR で作り、
  16 ビットへ展開してから `madd_epi16` で二乗和を取る。`Σb` は
  `sad_epu8(b, 0)` が 8 バイトごとの総和を u64 で返すのでそのまま使える
- 32 ビットレーンの累積は最悪でも約 2.7 億 (dim=66051、全要素 255) なので
  doc 記載の上限と整合する
- **検証はローカルではできない**。Apple Silicon の Rosetta 2 は AVX2 を
  公開しないため、x86_64 バイナリを動かしてもスカラー経路に落ちる。
  そこで `avx2_matches_scalar_exactly` を追加し、
  (a) AVX2 実装を名指しで呼ぶ (ディスパッチ経由だと素通りする)
  (b) `GITHUB_ACTIONS` が設定されているのに AVX2 が無ければ**失敗させる**
  ことで、CI (ubuntu = x86_64) での検証が形骸化しないようにした
- **性能は未測定**。x86_64 の実機が無いため、NEON のような倍率は出していない
  (正しさのみ保証)
