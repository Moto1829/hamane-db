# 502: extendCandidates のパラメータ化と構築コスト削減

- Status: DONE (2026-07-14)
- Milestone: M5
- Depends: 307 (計測ハーネス)
- Design: docs/design/index.md §1, docs/benchmarks.md (課題)

## ゴール

構築時の距離計算を約 2 倍にしている extendCandidates を必要な場合だけ
有効化できるようにし、自然データでの構築時間を削減する。

## やること

- [ ] `HnswParams.extend_candidates: bool` を追加 (既定は要計測で決定)
- [ ] SIFT1M で on/off の構築時間と recall を計測して既定値を決める
  - off で recall@10 ≥ 0.95 を維持できるなら既定 off
  - 強クラスタデータ用テスト (recall_at_10_clustered) は on 固定で維持
- [ ] ef_construction のスイープ (100/200/400) も同時に計測し、
      速度/再現率のトレードオフを docs/benchmarks.md に記録
- [ ] StoreOptions 経由で公開 (hamane::HnswParams は再エクスポート済み)

## 完了条件

- on/off × ef_construction の計測表が docs/benchmarks.md にある
- 既定値の根拠が計測で説明されている
- 既存の再現率テストすべて green
