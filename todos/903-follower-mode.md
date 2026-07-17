# 903: hamane-storage の follower モード

- Status: TODO
- Milestone: M9
- Depends: 901
- Design: docs/design/replication.md §4

## ゴール

replica 側で「読み取り専用 + 外部から供給される WAL フレームの適用 +
世代切り替え」ができる Store モードを追加する。

## やること

- [ ] `Store::open_follower(path)`: 書き込み API は ReadOnlyReplica エラー、
      自動フラッシュ・コンパクションなし、flock は取る
- [ ] `HamaneError::ReadOnlyReplica` の追加
- [ ] `apply_wal_frame(bytes)`: フレームを検証して follower memtable に適用
- [ ] `switch_generation()`: ディスク上の新 CURRENT を開き直して原子的に差し替え
      (旧 LiveView を持つ読者は Arc で安全に読み終えられる)
- [ ] 単体テスト: 適用順序 = リプレイと同一結果 / 切り替え中の検索安全性

## 完了条件

- 通常 open と follower open + apply が同じ入力から同じ検索結果になる
- 書き込み API が全て ReadOnlyReplica を返す
