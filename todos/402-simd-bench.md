# 402: SIMD 距離カーネルと criterion ベンチ

- Status: DONE (2026-07-12)
- Milestone: M4
- Depends: 307 (ベースライン計測があること)
- Design: DESIGN.md §4, crates/hamane-core/src/metric.rs

## ゴール

距離計算をアーキテクチャ別 SIMD で高速化し、退行を criterion で監視する。

## やること

- [ ] criterion ベンチ (`crates/hamane-core/benches/distance.rs`):
      l2_squared / dot、dim ∈ {64, 128, 768, 1536}
- [ ] aarch64: NEON intrinsics (`vfmaq_f32`)、x86_64: AVX2+FMA
      (`is_x86_feature_detected!` で実行時ディスパッチ)。フォールバックは現行実装
- [ ] unsafe 境界は距離カーネル内に閉じ込め、スカラー実装との一致を
      proptest で検証 (許容誤差 1e-4 相対)
- [ ] 現行の 4 レーン展開比で dim=768 において 2 倍以上を目標に計測・記録

## 完了条件

- 全プラットフォームでテスト green (スカラー一致)
- ベンチ結果 (before/after) を docs/benchmarks.md に追記
