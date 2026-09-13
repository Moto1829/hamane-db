# 1901: メタデータ専用の WAL レコード

- Status: TODO
- Milestone: M19
- Depends: 1701 (メタデータ更新 API), 1801 (WAL タグ追加の前例)
- Design: —

## ゴール

メタデータ更新の **WAL 書き込み量を次元に依存させない**。

1701 で `update_meta` を入れたが、内部は `upsert` に落としているので
WAL にはレコード全体 (ベクトル込み) が載る。dim=768 なら 1 件あたり
約 3 KB で、「100 万件にタグを付ける」と 3 GB を WAL に書くことになる。
メタデータだけなら数十バイトで済む。

WAL は fsync を伴う直列パスなので、ここが減ると一括更新のスループットに
直結する。

## やること

- [ ] `WalRecord::UpdateMeta { collection_id, id, metadata }` (タグ 7)。
      **差分ではなくマージ後の最終的なメタデータ**を載せる (リプレイが単純で
      冪等になる。差分だと適用順に依存する)
- [ ] `apply_record`: 対象の現在のレコードを解決し (memtable → pending →
      セグメント降順)、**ベクトルはそのまま**にメタデータだけ差し替えて
      memtable に載せる。対象が無ければ何もしない (削除済みとの競合)
- [ ] `Store::update_metadata(collection_id, id, metadata) -> Result<bool>` と
      `update_metadata_batch(...) -> Result<usize>` (sync は 1 回)
- [ ] `Collection::update_meta` / `update_meta_by_filter` の実装を差し替える
      (API と意味論は 1701 のまま変えない)
- [ ] `_ext_id` の追従: メタデータを差し替えても文字列 ID の対応が壊れないこと
      (マージ元に `_ext_id` が含まれるので保たれるはずだが、テストで固定する)
- [ ] `docs/spec/src/file-format.md` の WAL レコード表にタグ 7 を追加

## 完了条件

- [ ] メタデータ更新後の WAL 増分が**ベクトルのサイズに依存しない**
      (dim を変えて実測し、増分がほぼ一定であることを確認)
- [ ] 1701 のテストが**無変更で**通る (API の意味論を変えていない)
- [ ] WAL リプレイ後にメタデータ更新が復元される
      (セグメント上のレコードへの更新も含む)
- [ ] 更新対象が削除済みだった場合、リプレイで復活しない

## メモ

memtable 側のコストは変わらない (セグメント上のレコードを上書きするには、
ベクトルごと memtable に載せる必要がある)。**減るのは WAL の書き込み量だけ**。
