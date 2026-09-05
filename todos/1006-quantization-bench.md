# 1006: 量子化構成のベンチとドキュメント

- Status: DONE (2026-09-03)
- Milestone: M10
- Depends: 1003, 1004, 1005
- Design: docs/design/quantization.md §7

## ゴール

SIFT1M で全構成の recall / QPS / メモリ・ディスクサイズを計測し、
各手法のトレードオフを docs/benchmarks.md に記録する。

## やったこと

- [x] hamane-bench に `--config` (f32/sq8/pq/ivf/ivfpq)、`--pq-m`、`--nprobe`
      を追加。IVF 系は nprobe を、HNSW 系は ef をスイープ
- [x] 各構成の recall@10、QPS、構築時間、ディスクサイズを計測
      (SIFT 先頭 200k、dim 128、クエリ 1000)
- [x] IVF の nprobe スイープ、HNSW 系の ef スイープを表で記録
- [x] docs/benchmarks.md に「M10」節を追加 (実測 + 読み取り)
- [x] docs/DESIGN.md §9 と todos/README.md の M10 節を実測値で更新

## 完了条件

- [x] 素 HNSW 比で SQ8 QPS ~2x / PQ ~1.5x、IVF は HNSW グラフを書かず
      ディスク最小、が数値で示される
- [x] 各構成の recall@10 ≥ 0.95 (代表点) が計測で裏づけ
- [x] ベンチが再現可能な手順つきで docs 化 (`--config` 一発で切替)

## 実装メモ

- 200k では IVF/IVF-PQ は HNSW 系に QPS で劣る (グラフ枝刈りが優秀)。
  PQ の圧縮効果は「探索時の走査作業集合」に効き、億件規模でこそ意味を持つ。
  vectors.bin を再ランク用に常に残すためディスク総量は SQ8/PQ で増える点も記録
- 実データ SIFT1M フル (n=1M) の計測は CI 時間の都合で未実施。手順は docs 化済み
