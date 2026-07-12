# 401: コンパクション (size-tiered + tombstone GC)

- Status: DONE (2026-07-12)
- Milestone: M4 (完了ゲート)
- Depends: 209 (305 完了後なら HNSW 再構築も含む)
- Design: DESIGN.md §3, docs/design/storage.md §7

## ゴール

小さいセグメントをマージして tombstone を物理適用し、
長時間運用でディスク使用量とセグメント数が収束するようにする。

## やること

- [ ] トリガ: フラッシュ後に「同サイズ帯 (×4 区切り) のセグメントが 4 個以上」で発火
- [ ] マージ: 対象セグメント群を newest-wins で統合 (上書き・tombstone 適用) →
      新セグメントを SegmentWriter で書く (HNSW は VectorSource 経由で再構築)
- [ ] tombstone の保持規則: マージ対象より古いセグメントがまだある場合、
      その id の tombstone は新セグメントに引き継ぐ。全セグメントを含む
      full merge なら破棄
- [ ] manifest 更新 (旧セグメント除去 + 新セグメント追加) → 旧セグメント削除は
      検索中の Arc が全て drop された後 (Drop フックで遅延削除)
- [ ] v0 は flush と同じスレッドで同期実行 (バックグラウンド化は任意の発展)

## 完了条件

- upsert/delete を数十万件流し続けるテストで、セグメント数が有界・
  ディスク使用量が live データ量に比例して収束
- コンパクション前後で検索・get の結果が不変 (プロパティテスト 211 に
  Compact 操作を追加)
- 検索実行中にコンパクションしてもクラッシュ・不整合なし
