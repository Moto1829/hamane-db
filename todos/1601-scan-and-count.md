# 1601: レコードの列挙とカウント (scan / count)

- Status: TODO
- Milestone: M16
- Depends: 209 (読み取りパス)
- Design: —

## ゴール

**全件を列挙する手段が無い**のを解消する。現状レコードを取り出す方法は
`get(id)` (ID を知っている必要がある) と近傍検索 (クエリベクトルが要る) だけで、

- エクスポート / 別 DB への移行
- 「このメタデータを持つレコードを全部見たい」
- デバッグ時の中身確認

ができない。`len()` はあるが**条件つきの件数**も数えられない。

## やること

- [ ] `Collection::scan() -> ScanBuilder`
  - `.filter(Filter)` / `.limit(usize)` / `.after(id)` (カーソル)
  - `.run() -> Result<Vec<Record>>`
- [ ] **id 昇順**で返す (決定的。カーソル `after` によるページングが成立する)
- [ ] live なレコードだけ (tombstone と、新しいソースに shadow された行は除く)
- [ ] 各ソース (memtable / セグメント) は既に id 昇順なので、
      **k-way マージで limit に達したら打ち切る** (全件走査しない)
- [ ] `Collection::count(Option<&Filter>) -> Result<usize>`
      (フィルタなしは `len()` と一致)
- [ ] HTTP: `GET /collections/{name}/records?limit=&after=&filter=`
- [ ] CLI: `hamane scan <db> <collection> [--limit] [--after] [--filter]`
- [ ] Python: `col.scan(limit=, after=, filter=)` / `col.count(filter=)`

## 完了条件

- [ ] 全件列挙が実データと一致する (削除・上書き後も、`get` の結果と一致)
- [ ] `after` + `limit` のページングで**重複も取りこぼしも無い**
- [ ] フィルタつき `count` が総当たりの数と一致する
- [ ] 大きな collection で `limit` が効く (全件走査していないことを確認)
