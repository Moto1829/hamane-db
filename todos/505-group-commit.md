# 505: WAL group commit (SyncPolicy::Batch)

- Status: TODO
- Milestone: M5
- Depends: なし
- Design: docs/design/storage.md §2 (SyncPolicy::Batch は設計済み未実装)

## ゴール

`SyncPolicy::Always` (書き込みごと fsync) と `EveryN` (耐久性が弱い) の間を
埋める group commit を実装し、耐久性を保ったまま書き込みスループットを上げる。

## やること

- [ ] `SyncPolicy::Batch { max_delay: Duration }`: 書き込みは fsync 待ちの
      キューに入り、直近の fsync から max_delay 以内にまとめて 1 回 fsync。
      **fsync 完了までは呼び出し元に Ok を返さない** (= ack 済みは常に永続)
- [ ] 実装: fsync 専用スレッド + condvar。呼び出し元は自分の書き込みを含む
      fsync 世代の完了を待つ
- [ ] ベンチ: 並行 8 スレッドの upsert スループットを Always / Batch(1ms) /
      EveryN で比較し docs/benchmarks.md に記録
- [ ] WAL 切り詰めテスト (210) が Batch でも green なことを確認
      (ack 済みレコードが必ず復元される)

## 完了条件

- Batch(1ms) が Always 比で並行書き込みスループット 5 倍以上 (SSD 想定)
- 「Ok が返った書き込みはクラッシュ後も残る」テストが Batch で green
