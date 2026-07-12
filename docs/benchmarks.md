# ベンチマーク記録

## 距離カーネル (todos/402)

- 日付: 2026-07-12
- 環境: Apple Silicon (aarch64, NEON) / macOS / rustc 1.93.0
- 実行: `cargo bench -p hamane-core --bench distance -- --quick`
- scalar = 4 レーン展開のスカラー実装 (自動ベクトル化任せ) / simd = NEON intrinsics

| dim | l2 scalar | l2 simd | 倍率 | dot scalar | dot simd | 倍率 |
|---|---|---|---|---|---|---|
| 64 | 25.0 ns | 5.5 ns | 4.5x | 22.9 ns | 5.4 ns | 4.2x |
| 128 | 43.2 ns | 11.1 ns | 3.9x | 39.1 ns | 12.1 ns | 3.2x |
| 768 | 209 ns | 126 ns | 1.7x | 211 ns | 97.5 ns | 2.2x |
| 1536 | — | 213 ns | — | — | — | — |

備考: 高次元ではメモリ帯域が支配的になり倍率が縮む (dim768 の l2 は目標 2x に
対し 1.7x)。x86_64 (AVX2+FMA) は実行時ディスパッチ実装済みだが未計測。

## SIFT1M (todos/307)

- 日付: 2026-07-12
- 環境: Apple Silicon (aarch64) / macOS / rustc 1.93.0 / release ビルド / 単一スレッド
- 実行: `./scripts/download_sift1m.sh && cargo run --release -p hamane-bench -- --data data/sift`
- 構成: n=1,000,000 (dim 128, L2)、クエリ 10,000 本、正解は配布の ground truth
- HNSW 既定パラメータ (m=16, m0=32, ef_construction=200)、単一セグメント

| 項目 | 値 |
|---|---|
| 挿入 (WAL + memtable) | 3.4 s (298k rec/s) |
| フラッシュ + HNSW 構築 | 1438.8 s (~24 分, 695 rec/s) |
| ディスクサイズ | 650 MB (ベクトル 512MB + HNSW ~130MB) |

| ef | recall@10 | QPS (1 thread) | 平均レイテンシ |
|---|---|---|---|
| 16 | 0.8487 | 8508 | 0.12 ms |
| 32 | 0.9328 | 6243 | 0.16 ms |
| **64 (既定)** | **0.9772** | **3850** | **0.26 ms** |
| 128 | 0.9939 | 2273 | 0.44 ms |
| 256 | 0.9984 | 1308 | 0.76 ms |

**M3 完了条件 (recall@10 ≥ 0.95) を既定 ef=64 で達成 (0.977)。**

課題 (将来の最適化候補):

- HNSW 構築が単一スレッドで ~24 分。hnswlib はマルチスレッドで分単位。
  挿入の並列化 (ノード単位ロック) が最大の改善余地
- extendCandidates 常時有効の構築コスト (距離計算 ~2 倍)。SIFT のような
  自然データではオフでも再現率が出る可能性があり、パラメータ化を検討
