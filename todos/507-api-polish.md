# 507: API 品質の小改善バックログ

- Status: DONE (2026-07-14)
- Milestone: M5
- Depends: なし
- Design: 実装中に見つかった小課題の集約

## ゴール

単体では 1 タスクに満たない改善をまとめて解消する。

## やること

- [ ] `Collection::delete` の「existed 判定 → 削除」が 2 回のロック取得で
      非原子的 (並行 upsert と競合すると戻り値が不正確)。Store 側に
      `delete(id) -> bool` を用意して 1 臨界区間にする
- [ ] `HnswParams` / `StoreOptions` の値検証 (m == 0, ef == 0,
      flush_threshold == 0 等で panic せずエラー)
- [ ] `Metric::Cosine` / `Dot` の再現率テスト追加 (現状 recall テストは L2 のみ。
      正規化済みベクトルでの HNSW 品質を確認)
- [ ] 再 open 時の `CollectionConfig` と `create_collection` 引数の不一致検出
      (同名 collection を違う dim で開き直そうとしたら明示エラー)
- [ ] `hamane` クレートの rustdoc 整備 (`#![warn(missing_docs)]` を通す)
- [ ] CLI: `hamane info` にセグメント構成 (数・行数・hnsw 有無) を表示
      (todo 403 の残項目)

## 完了条件

- 各項目にテストがあり、全テスト green
- `cargo doc -p hamane` が警告なしで通る
