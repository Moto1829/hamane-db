# 1203: 4-bit PQ のベンチとドキュメント

- Status: DONE (2026-09-06)
- Milestone: M12
- Depends: 1202
- Design: docs/design/quantization.md §8

## ゴール

同じコード長で「m 倍 × 4bit」と「m × 8bit」を比べ、メモリ/recall の
トレードオフを記録する。

## やること

- [x] `hamane-bench` に `--pq-nbits` を追加
- [x] SIFT 200k で (m=32,8bit) / (m=32,4bit) / (m=64,4bit) を計測
- [x] docs/benchmarks.md に M12 節、docs/spec と設計文書を更新

## 完了条件

- [x] 同じコード長 (m=64,4bit = m=32,8bit = 32B/行) での recall 比較がある
- [x] 半分のコード長 (m=32,4bit = 16B/行) の recall 低下量が記録される

## 実装メモ

- **同じコード長 (32 B/行) なら 4bit × m=64 が 8bit × m=32 より良い**:
  recall 0.9863 vs 0.9838、構築 32.7 s vs 76.0 s (2.3 倍速)。k-means の
  セントロイドが 16 個で済むコスト削減が m 倍増を上回る
- 代償は QPS (9402 → 6391)。表引き回数が倍でニブル展開も入る。
  PQ4 の SIMD fast-scan が将来の最適化余地 (未タスク化)
- m 据え置きの 4bit (16 B/行) は recall 0.907 で、**ef を上げても改善しない**。
  律速が 1 段目の粗さであることが ef スイープから読み取れる
