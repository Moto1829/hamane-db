# 501: HNSW 構築の並列化

- Status: TODO
- Milestone: M5
- Depends: なし
- Design: docs/design/index.md §1, docs/benchmarks.md (課題)

## ゴール

SIFT1M の HNSW 構築 1439 秒 (単一スレッド) を、マルチコアで 5 分以内に短縮する。
これは現状プロジェクト最大の性能ボトルネック (hnswlib はマルチスレッドで分単位)。

## やること

- [ ] hnswlib 方式の並列挿入: ノード単位のロック (`Vec<Mutex<()>>` 相当) で
      隣接リストの更新を保護し、複数スレッドが同時に insert する
  - visited ビットセットはスレッドローカルに持つ (現状は insert ごとに確保)
  - entry_point / max_level の更新は CAS または全体ロックの短い臨界区間で
- [ ] `HnswParams.build_threads: usize` (既定 = 物理コア数、1 で従来動作)
- [ ] 並列構築は挿入順が非決定になるため、**決定性の扱いを明示する**:
      `build_threads == 1` のときのみ決定的 (deterministic_build テストは 1 固定)。
      セグメントの「同一入力 → 同一バイト列」保証は失われる旨を
      docs/design/storage.md の決定性の記述に追記
- [ ] rayon は使わず std::thread + チャネル or スコープドスレッドで
      (依存を増やさない。粒度が大きいので work-stealing 不要)

## 完了条件

- SIFT1M (`cargo run --release -p hamane-bench`) の構築が 8 コアで 300 秒以内
- 並列構築でも recall@10 ≥ 0.95 (既定 ef=64) を維持
- 単一スレッド時の全既存テスト green (構造不変条件テストは並列でも green)
- docs/benchmarks.md に before/after を追記
