# 801: 検索スレッドプール化

- Status: DONE (2026-07-15)
- Milestone: M8
- Depends: 503
- Design: docs/design/query.md §2

## ゴール

503 のセグメント並列検索は検索のたびに `std::thread::scope` でスレッドを
生成・破棄している。tiered compaction (506) 後はセグメント数が
compaction_threshold を超え得るため、生成コストと無制限の並列度が
レイテンシのばらつき (特に hamane-server の同時検索) につながる。
Database 全体で共有する常駐スレッドプールに置き換え、
スレッド生成コストの排除と並列度の上限設定を可能にする。

## やること

- [x] `StoreOptions.search_threads: usize` を追加 (0 = 自動 =
      available_parallelism。build_threads と同じ規約)
- [x] hamane クレートに std のみの固定サイズプール (mpsc + Mutex<Receiver>) を
      実装。初回の複数セグメント検索まで worker は起動しない (遅延初期化)。
      ジョブの panic は worker を殺さず呼び出し元へ再伝播する
- [x] `run_search` のセグメント検索を 'static ジョブ化 (LiveView / query /
      filter を owned なコンテキスト構造体 SegmentSearch に集約し Arc で共有)。
      呼び出しスレッドも 1 セグメントを担当し、実効並列度 = search_threads
- [x] `search_threads == 1` は逐次実行 (プール不使用)。セグメント 1 個以下も
      従来どおりインライン
- [x] 計測: 複数セグメント構成で thread::scope 版と QPS / レイテンシ比較
      (docs/benchmarks.md M8 参照。QPS +14%)
- [x] docs/design/query.md §2 の並列化の記述を更新 (仕様書の
      configuration.md / search.md も追記)

## 完了条件

- 既存テスト (crash / proptest 含む) green — **達成**
- 複数セグメント検索の QPS が thread::scope 版と同等以上 — **達成 (+14%。
  SIFT 200k / 2 セグメント / ef=64 で 4.6k → 5.2k QPS、0.217 → 0.190 ms)**
- 同時多発検索でスレッド数が search_threads + 呼び出し元数に有界 —
  **達成 (worker は search_threads − 1 本固定。テスト
  concurrent_searches_are_consistent で動作確認)**

## 実装メモ

- 計測は同一 DB への交互実行で行うこと。DB を作り直すとバックグラウンド
  フラッシュのタイミングでセグメント構成が変わり、QPS が run 間で
  2 倍以上ぶれる (今回それで一度「プールが遅い」と誤判定しかけた)
- プールは mpsc + `Mutex<Receiver>` の素朴な構成で十分だった。
  spin-before-park はマイクロベンチではさらに速いが複雑さに見合わず見送り
