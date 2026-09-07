# ベンチマーク記録

## M8: 検索スレッドプール化 (todo 801, 2026-07-15)

環境: Apple Silicon (aarch64) / macOS / release ビルド。
SIFT 200k (dim 128, L2)、セグメント 2 個 (110k + 90k、HNSW あり)、
クエリ 1,000 本、k=10、ef=64、クライアント 1 スレッド。

計測方法: DB 構築はバックグラウンドフラッシュのタイミングでセグメント構成が
run ごとに変わり比較にならないため、**同一 DB** に対し新旧バイナリを
交互に 3 回ずつ (各 3 ラウンド) 実行して比較した。両者の検索結果は
checksum 一致。

| 実装 | QPS (1 thread) | 平均レイテンシ |
|---|---|---|
| thread::scope (検索ごとに生成、503) | 4541〜4655 | 0.217 ms |
| 共有プール (801) | **5201〜5294** | **0.190 ms** |

約 14% のスループット向上 (クエリあたり約 27µs のスレッド生成コストが消えた)。
ディスパッチ単体のマイクロベンチ (150µs のジョブ × 3、うち 1 個は呼び出し
スレッドが実行) でも scope 123µs → プール 78µs。

## M7: SQ8 の u8 SIMD カーネル (todo 701, 2026-07-15)

NEON (aarch64)、dim=768。整数演算のためスカラーと完全一致:

| カーネル | f32 SIMD | SQ8 SIMD | 倍率 |
|---|---|---|---|
| L2 | 90.2 ns | **21.5 ns** | 4.2x |
| dot (+Σb) | 88.7 ns | **23.3 ns** | 3.8x |

完了条件 (f32 SIMD の 2 倍以上) を達成。x86_64 はスカラーのまま
(自動ベクトル化任せ。AVX2 実装は将来候補)。
u32 アキュムレータのため dim ≤ 66051 が前提。

## M5 の改善結果 (2026-07-14)

環境: Apple Silicon (aarch64) / macOS / rustc 1.93.0 / release ビルド。

### HNSW 構築の並列化 (todo 501)

SIFT1M (100 万 × 128 次元)、`cargo run --release -p hamane-bench -- --data data/sift`:

| 構成 | 構築時間 | recall@10 (ef=64) |
|---|---|---|
| M4 (単一スレッド) | 1438.8 s | 0.977 |
| 単一スレッド + HashSet 修正 | 1377.8 s | 0.972 |
| **並列 (auto = 全コア)** | **297.7 s (4.8x)** | **0.972** |

完了条件 (300 秒以内・recall 維持) を達成。`build_threads: 1` のときのみ構築は決定的。

### extendCandidates の計測 (todo 502)

SIFT 200k サブセット、ef=64:

| 構成 | 構築 | recall@10 |
|---|---|---|
| extend ON (既定) | 50.9 s | 0.9897 |
| extend OFF | 40.6 s (−20%) | 0.9881 |
| extend OFF + ef_construction=300 | 44.7 s | 0.9891 |

SIFT のような自然データでは OFF で構築 20% 高速・recall ほぼ同等。
ただし強くクラスタ化したデータでは OFF だと recall が 0.84 まで落ちる
(recall_at_10_clustered テスト) ため、**既定は ON を維持**。
`HnswParams.extend_candidates = false` で opt-out できる。

### バックグラウンドフラッシュ中の書き込みレイテンシ (todo 504)

`cargo run --release -p hamane --example write_latency`
(200k upsert、16MiB 閾値で自動フラッシュを誘発、fsync 除外):

| 指標 | 値 |
|---|---|
| スループット | 141k upsert/s |
| p50 / p99 / p99.9 | 2.6µs / **8µs** / 48µs |
| max | 675ms (backpressure: active が閾値 4 倍到達時のみ) |

完了条件 (フラッシュ中の upsert p99 < 10ms) を達成。M4 まではフラッシュ
(HNSW 構築込み、分単位) の間すべての書き込みが停止していた。


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

## M10: 量子化とクラスタリング索引 (todos/1006)

- 日付: 2026-09-03
- 環境: Apple Silicon (aarch64) / macOS / rustc 1.93.0 / release / 単一スレッド
- 実行: `cargo run --release -p hamane-bench -- --limit 200000 --queries 1000 --config <c>`
- 構成: SIFT の先頭 n=200,000 (dim 128, L2)、クエリ 1,000 本、正解は総当たり
- HNSW 既定パラメータ、単一セグメント。PQ は m=32 (dsub=4)、IVF は nlist≈447 (√n)

各構成の代表点 (HNSW 系は ef、IVF 系は nprobe のスイープから recall≈0.99 付近):

| 構成 | 索引 | recall@10 | QPS | 構築 | ディスク |
|---|---|---|---|---|---|
| f32 | HNSW | 0.9894 (ef=64) | 4799 | 32.9 s | 130 MB |
| SQ8 | HNSW + SQ8 | 0.9894 (ef=64) | **9531** | 33.8 s | 154 MB |
| PQ | HNSW + PQ | 0.9835 (ef=64) | 7043 | 89.2 s | 136 MB |
| IVF | IVF | 0.9896 (nprobe=32) | 378 | 31.8 s | **104 MB** |
| IVF-PQ | IVF + 残差PQ | 0.9852 (nprobe=32) | 728 | 86.5 s | 110 MB |

読み取り:

- **量子化 (SQ8/PQ) は HNSW 探索を高速化する。** 1 段目の距離計算が u8/表引きに
  なり、SQ8 で QPS 約 2 倍、PQ で約 1.5 倍。recall は f32 再ランクでほぼ同等
  (SQ8 は完全同等、PQ は僅かに低下)
- **IVF は同 recall で HNSW より遅い** (200k では HNSW グラフの枝刈りが優秀)。
  IVF の価値は構築の単純さと、億件規模でのメモリ特性。IVF-PQ は ADC の表引きで
  IVF 比 QPS 約 2 倍
- **ディスク**: 再ランク用に vectors.bin (512 B/行) を常に残すため、SQ8/PQ は
  量子化ファイルが**加算**されディスクは増える。PQ の圧縮効果 (codes 32 B/行 =
  f32 比 16x) が効くのは「探索時に走査する作業集合」であり、億件規模でこそ意味を
  持つ。IVF は HNSW グラフ (~26 MB) を書かないぶんディスク最小
- 構築時間: PQ / IVF-PQ はコードブック学習 (サブベクトル 32 本 × k-means) が
  加わり f32 の約 2.7 倍。IVF (量子化なし) は粗 k-means のみで f32 と同等

**結論**: 中規模・低レイテンシ重視なら HNSW+SQ8 が最良 (recall 同等・QPS 2倍)。
メモリ制約が支配的な超大規模では PQ / IVF-PQ が作業集合を 16x 圧縮する。
用途に応じて `StoreOptions.{index, quantization, nprobe}` で選択する。
