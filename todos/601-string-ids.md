# 601: 文字列 ID 対応

- Status: TODO
- Milestone: M6
- Depends: なし (フォーマット変更を伴うため 506 の v2 bump と同時が望ましい)
- Design: DESIGN.md §2 (「id は u64 or string」の string 側が未実装)

## ゴール

外部システムの ID (UUID 等) をそのまま使えるよう、u64 に加えて
文字列 ID をサポートする。埋め込み用途では最頻出の要望。

## やること

- [ ] 方式を決める。推奨: **内部 ID は u64 のまま**、collection ごとに
      「外部文字列 ID → 内部 u64」の辞書を持つ
  - 距離計算・セグメント・HNSW・tombstone は一切変更不要
  - 辞書は WAL に載せ、フラッシュ時にセグメントへ (`extid.bin`:
    ソート済み文字列 + 内部 id のペア)
- [ ] 公開 API: `Id` を enum にはせず、`Record::new(impl Into<RecordId>, ...)`
      で `u64 | &str | String` を受ける。u64 のみの既存コードは無変更で通る
- [ ] 文字列 ID 使用時の SearchHit / get の返却 ID の扱いを決めて実装
- [ ] proptest に文字列 ID の系列を追加

## 完了条件

- 既存の u64 API が後方互換 (既存テスト無修正で green)
- 文字列 ID で upsert / get / delete / search / 再 open が動く統合テスト
- 辞書のクラッシュ耐性 (WAL リプレイで復元) テスト
