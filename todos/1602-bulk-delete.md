# 1602: 条件による一括削除

- Status: TODO
- Milestone: M16
- Depends: 1601
- Design: —

## ゴール

「テナント X のデータを全部消す」を 1 回で表現できるようにする。
現状の削除は `delete(id)` だけなので、呼び出し側が ID を集めてループするしかない
(しかも列挙手段が無かった = 1601)。1 件ずつだと WAL の fsync も件数ぶん走る。

## やること

- [ ] `Collection::delete_batch(ids) -> Result<usize>`
      (削除できた件数。WAL は 1 回にまとめて sync する。`upsert_batch` と対称)
- [ ] `Collection::delete_by_filter(&Filter) -> Result<usize>`
      (1601 の走査で一致 ID を集めて `delete_batch`)
- [ ] 大量削除でメモリを食い潰さないよう、内部ではチャンクに区切って処理する
- [ ] HTTP: `DELETE /collections/{name}/records` (body に filter)
- [ ] CLI: `hamane delete <db> <collection> --filter '<JSON>'`
- [ ] Python: `col.delete_batch(ids)` / `col.delete_by_filter(filter)`

## 完了条件

- [ ] 一致するものだけが消え、件数が返る (`count` と整合)
- [ ] 一致 0 件でエラーにならず 0 が返る
- [ ] 削除後に `scan` / `get` / 検索のどれからも見えない
- [ ] クラッシュ耐性は既存と同じ (WAL に載ってから ack)
