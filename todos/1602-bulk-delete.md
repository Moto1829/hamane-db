# 1602: 条件による一括削除

- Status: DONE (2026-09-13)
- Milestone: M16
- Depends: 1601
- Design: —

## ゴール

「テナント X のデータを全部消す」を 1 回で表現できるようにする。
現状の削除は `delete(id)` だけなので、呼び出し側が ID を集めてループするしかない
(しかも列挙手段が無かった = 1601)。1 件ずつだと WAL の fsync も件数ぶん走る。

## やること

- [x] `Collection::delete_batch(ids) -> Result<usize>`
      (削除できた件数。WAL は 1 回にまとめて sync する。`upsert_batch` と対称)
- [x] `Collection::delete_by_filter(&Filter) -> Result<usize>`
      (1601 の走査で一致 ID を集めて `delete_batch`)
- [x] 大量削除でメモリを食い潰さないよう、内部ではチャンクに区切って処理する
- [x] HTTP: `DELETE /collections/{name}/records` (body に filter)
- [x] CLI: `hamane delete <db> <collection> --filter '<JSON>'`
- [x] Python: `col.delete_batch(ids)` / `col.delete_by_filter(filter)`

## 完了条件

- [x] 一致するものだけが消え、件数が返る (`count` と整合)
- [x] 一致 0 件でエラーにならず 0 が返る
- [x] 削除後に `scan` / `get` / 検索のどれからも見えない
- [x] クラッシュ耐性は既存と同じ (WAL に載ってから ack)

## 実装メモ

- `Store::delete_batch` は `upsert_batch` と同じ形: 1 臨界区間で N 件の
  Delete を WAL に append → **sync は 1 回** → memtable に反映。
  戻り値は「実際に live だった件数」なので、存在しない ID は数えない
- `delete_by_filter` は 1 万件ずつ走査して削除するループ。
  削除しながら走査すると view がずれるので、**1 チャンク集めてから消す**
- CLI の `delete` は `--filter` を必須にした (誤って全件消すのを防ぐ)。
  HTTP も body の `filter` が必須
