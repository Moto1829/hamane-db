# 302: HNSW 探索

- Status: DONE (2026-07-12)
- Milestone: M3
- Depends: 301
- Design: docs/design/index.md §1

## ゴール

構築済み HNSW グラフに対する k-NN 探索を実装する (ビルダー / mmap ビュー共通)。

## やること

- [ ] グラフアクセスを trait `HnswGraph` (`levels`, `neighbors(level, node)`,
      `entry_point`) に切り出し、`HnswBuilder` に実装
      (304 の `HnswView` も同 trait を実装する前提の設計)
- [ ] `search(graph, source, query, k, ef, filter_mask) -> Vec<(row, key)>`:
  - 最上層→1 層を ef=1 で降下、層 0 で `search_layer(max(ef, k))`
  - `filter_mask: Option<&dyn Fn(u32) -> bool>` は**結果採用のみ**マスク
    (走査は全ノード。index.md §1 の根拠を doc comment に)
- [ ] `Hit` への変換は呼び出し側 (score_from_key)

## 完了条件

- n=1000, ef=n 相当の設定で Flat と完全一致 (探索が全域に届く極限の健全性)
- filter_mask で除外した row が結果に現れない
- k > ノード数 / 空グラフ / entry_point のみ、のエッジケース
