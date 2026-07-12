# 301: HNSW 構築 (挿入・隣接選択)

- Status: DONE (2026-07-12)
- Milestone: M3
- Depends: なし (hamane-index 内で独立に進められる)
- Design: docs/design/index.md §1

## ゴール

memtable / セグメントのどちらからでも構築できるインメモリ HNSW ビルダーを実装する。

## やること

- [ ] `HnswParams { m, m0, ef_construction, ef_search, seed }` + Default (index.md の既定値)
- [ ] `VectorSource` trait (`fn len() -> u32` / `fn vector(row: u32) -> &[f32]`)
- [ ] `HnswBuilder::build(source, metric, params)`:
  - レベル抽選 (`-ln(U) * ml`, StdRng seed 固定可)
  - greedy 降下 + `search_layer(q, ef_construction)` (Algorithm 1/2)
  - ヒューリスティック隣接選択 (Algorithm 4) と逆向きエッジの刈り込み
- [ ] 距離は `Metric::distance_key` のみ使用 (メトリック非依存)
- [ ] visited 集合はビットセット (`Vec<u64>`)

## 完了条件

- 小規模データ (n=100) で: 全ノードが層 0 に存在、接続数上限 (m/m0) 遵守、
  entry_point から全ノードへ到達可能 (BFS) のユニットテスト
- seed 固定で 2 回構築した結果が完全一致 (決定性)
