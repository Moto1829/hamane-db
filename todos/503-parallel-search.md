# 503: 検索のソース並列化と live_len の O(1) 化

- Status: TODO
- Milestone: M5
- Depends: なし
- Design: docs/design/query.md §2 (「ソース並列化は M4」とした残件)

## ゴール

複数セグメントを持つ collection の検索レイテンシを改善し、
`len()` の O(総行数) 走査をなくす。

## やること

- [ ] `run_search` のセグメントごとの探索を並列化
      (std::thread::scope。セグメント数は高々 compaction_threshold なので
      スレッドプール不要)
- [ ] 計測: セグメント 4 個 (コンパクション直前) の状態で並列化前後の
      レイテンシを hamane-bench の `--flush-threshold` オプションで比較
- [ ] `live_len` の O(1) 化: フラッシュ/コンパクション時に「このセグメントの
      live 行数」を確定できないか検討。厳密な維持が複雑なら、manifest に
      「上限値 (record_count 合計)」と「正確な値のキャッシュ」を持ち、
      dirty なら遅延再計算する方式でよい
- [ ] `Collection::len()` の doc comment を実装に合わせて更新

## 完了条件

- 4 セグメント構成でマルチコアの検索スループット向上を計測・記録
- len() が「フラッシュ直後の呼び出しで O(1)」になる
- 既存テスト (proptest の len 比較含む) green
