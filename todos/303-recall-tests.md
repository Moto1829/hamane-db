# 303: 再現率テスト (recall@10 ≥ 0.95)

- Status: DONE (2026-07-12)
- Milestone: M3
- Depends: 302
- Design: docs/design/index.md §2

## ゴール

既定パラメータの HNSW が十分な再現率を持つことを CI で継続的に担保する。

## やること

- [ ] テストデータ生成: seed 固定で (a) 一様乱数 (b) ガウス混合クラスタ、
      n=10_000、dim ∈ {64, 512}、クエリ 100 本
- [ ] recall@10 = |HNSW 上位 10 ∩ Flat 上位 10| / 10 の平均を計算
- [ ] 既定パラメータ (m=16, ef_construction=200, ef_search=64) で
      全組み合わせ recall@10 ≥ 0.95 をアサート
- [ ] ef_search を 16→256 に振って recall が単調に上がることも確認
      (探索実装の破れを検出しやすい)
- [ ] 実行時間が CI 許容内 (数十秒) に収まるよう `--release` プロファイルの
      テスト実行を検討 (`[profile.test] opt-level = 2` でも可)

## 完了条件

- 上記テストが CI で安定して green (flaky でない = seed 完全固定)
