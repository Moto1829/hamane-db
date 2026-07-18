# 903: hamane-storage の follower モード

- Status: DONE (2026-07-18)
- Milestone: M9
- Depends: 901
- Design: docs/design/replication.md §4

## ゴール

replica 側で「読み取り専用 + 外部から供給される WAL フレームの適用 +
世代切り替え」ができる Store モードを追加する。

## やること

- [x] `Store::open_follower(path)`: 書き込み API は ReadOnlyReplica エラー、
      自動フラッシュ・コンパクションなし、flock は取る
- [x] `HamaneError::ReadOnlyReplica` の追加
- [x] `apply_wal_frame(bytes)`: フレームを検証して follower memtable に適用
- [x] `switch_generation()`: ディスク上の新 CURRENT を開き直して原子的に差し替え
      (旧 LiveView を持つ読者は Arc で安全に読み終えられる)
- [x] 単体テスト: 適用順序 = リプレイと同一結果 / 切り替え中の検索安全性

## 実装メモ

- `apply_wal_frame` は設計から `apply_wal_frames(bytes) -> usize` (消費バイト数
  返却) に変更。チャンク境界の持ち越しを呼び出し側が扱いやすい
- open の手順 1〜5 を `load_state` に切り出して switch_generation と共用
- Store の Drop (Shutdown) は pending のみフラッシュし active memtable は
  フラッシュしない — テストで gen を進めるには明示 flush が必要 (ハマった)

## 完了条件 — 達成 (単体テスト 5 本 green)

- 通常 open と follower open + apply が同じ入力から同じ検索結果になる
- 書き込み API が全て ReadOnlyReplica を返す
