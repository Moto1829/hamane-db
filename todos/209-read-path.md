# 209: 複数ソース読み取り (LiveView, newest-wins)

- Status: DONE (2026-07-12)
- Milestone: M2
- Depends: 208
- Design: docs/design/storage.md §7, docs/design/query.md §1–2

## ゴール

memtable + 複数セグメントにまたがるデータを、更新・削除の意味論を保って
検索・点参照できるようにする。M2 の検索はすべて Flat。

## やること

- [ ] `LiveView { memtable_snapshot, segments (seg_id 降順) }` と
      `is_live(id, source_rank)` (storage.md §7)
- [ ] `get(id)`: memtable → セグメント降順の優先解決 (tombstone 考慮)
- [ ] `run_search`: 各ソースで search_flat → newest-wins dedupe → k 件マージ
      (query.md §2 のフロー。dedupe で k 件未満になり得る旨を doc comment に明記)
- [ ] `len()` の意味を「live なレコード数」に再定義
      (manifest の record_count 合計 − 重複 − tombstone。厳密計算が重ければ
      フラッシュ時に確定値を manifest に持たせる)
- [ ] 検索開始時のスナップショット取得をロック外実行に (query.md §1)

## 完了条件

- 「セグメントの値を memtable の upsert が上書き」「セグメントの値を
  tombstone が消す」「2 セグメント間の新旧」の 3 意味論の統合テスト
- flush を挟んでも tests/api.rs 相当のシナリオが全 green
- 並行テスト: 読み 4 スレッド + 書き 1 スレッドで panic / 不整合なし
