# 701: SQ8 の u8 SIMD カーネル

- Status: DONE (2026-07-15)
- Milestone: M7
- Depends: 602
- Design: crates/hamane-core/src/sq8.rs (602 の残課題)

## ゴール

SQ8 の距離計算 (sq8_l2_accum / sq8_dot_accum) を NEON / AVX2 で高速化し、
SQ8 経路の検索スループットを f32 経路より速くする。

## やること

- [ ] NEON (aarch64): vabd (絶対差) + vmull_u8 + vpadal の widening 累積
- [ ] AVX2 (x86_64): maddubs 系。実行時ディスパッチは f32 カーネルと同じ方式
- [ ] u32 アキュムレータのオーバーフロー境界 (dim ≤ 66051) を doc に明記
- [ ] スカラー実装との完全一致テスト (整数演算なので誤差ゼロで比較)
- [ ] criterion ベンチ (distance.rs に追加) で f32 SIMD と比較

## 完了条件

- 全プラットフォームでスカラー一致テスト green
- dim=768 で SQ8 SIMD が f32 SIMD (NEON) の 2 倍以上のスループット
- docs/benchmarks.md に記録
