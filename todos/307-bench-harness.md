# 307: SIFT1M ベンチハーネス

- Status: DONE (2026-07-12)
- Milestone: M3 (完了ゲート)
- Depends: 305
- Design: DESIGN.md §8–9

## ゴール

公開データセットで recall / QPS / 構築時間を計測し、M3 完了条件
(SIFT1M recall@10 ≥ 0.95) を実測で確認する。

## やること

- [ ] `crates/hamane-bench` (または hamane-cli のサブコマンド) を追加。CI 対象外
- [ ] SIFT1M (.fvecs/.ivecs) のダウンロード (スクリプト) とパーサ
- [ ] 計測: 全件 upsert (+flush) → 10k クエリで recall@10 / QPS (単一スレッド &
      並列) / 構築時間 / ディスクサイズ を表形式で出力
- [ ] ef_search ∈ {16, 32, 64, 128, 256} のスイープ
- [ ] 結果を `docs/benchmarks.md` に記録するテンプレート

## 完了条件

- SIFT1M で既定パラメータ recall@10 ≥ 0.95 を実測 → M3 完了
- 結果が docs/benchmarks.md に記録されている
