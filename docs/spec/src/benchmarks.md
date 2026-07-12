# ベンチマーク

計測環境: Apple Silicon (aarch64) / macOS / rustc 1.93.0 / release ビルド /
単一スレッド。再現手順はリポジトリの `docs/benchmarks.md` と
`crates/hamane-bench` を参照。

## SIFT1M (100 万件, 128 次元, L2)

クエリ 10,000 本、正解は配布のグラウンドトゥルース。
HNSW は既定パラメータ (m=16, ef_construction=200)。

| 項目 | 値 |
|---|---|
| 挿入 (WAL + memtable) | 3.4 s (約 30 万 rec/s) |
| フラッシュ + HNSW 構築 | 1438.8 s (単一スレッド) |
| ディスクサイズ | 650 MB |

### ef と再現率・速度のトレードオフ

| ef | recall@10 | QPS (1 thread) | 平均レイテンシ |
|---|---|---|---|
| 16 | 0.849 | 8508 | 0.12 ms |
| 32 | 0.933 | 6243 | 0.16 ms |
| **64 (既定)** | **0.977** | **3850** | **0.26 ms** |
| 128 | 0.994 | 2273 | 0.44 ms |
| 256 | 0.998 | 1308 | 0.76 ms |

## 距離カーネル (SIMD)

NEON (aarch64) とスカラー実装の比較:

| dim | L2 スカラー | L2 SIMD | 倍率 | dot スカラー | dot SIMD | 倍率 |
|---|---|---|---|---|---|---|
| 64 | 25.0 ns | 5.5 ns | 4.5x | 22.9 ns | 5.4 ns | 4.2x |
| 128 | 43.2 ns | 11.1 ns | 3.9x | 39.1 ns | 12.1 ns | 3.2x |
| 768 | 209 ns | 126 ns | 1.7x | 211 ns | 97.5 ns | 2.2x |

高次元ではメモリ帯域が支配的になり倍率が縮みます。

## 再現手順

```sh
./scripts/download_sift1m.sh                       # ~161MB ダウンロード
cargo run --release -p hamane-bench -- --data data/sift
cargo bench -p hamane-core                         # 距離カーネル
```
