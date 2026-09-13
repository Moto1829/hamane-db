# 1601: レコードの列挙とカウント (scan / count)

- Status: DONE (2026-09-13)
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

- [x] `Collection::scan() -> ScanBuilder`
  - `.filter(Filter)` / `.limit(usize)` / `.after(id)` (カーソル)
  - `.run() -> Result<Vec<Record>>`
- [x] **id 昇順**で返す (決定的。カーソル `after` によるページングが成立する)
- [x] live なレコードだけ (tombstone と、新しいソースに shadow された行は除く)
- [x] 各ソース (memtable / セグメント) は既に id 昇順なので、
      **k-way マージで limit に達したら打ち切る** (全件走査しない)
- [x] `Collection::count(Option<&Filter>) -> Result<usize>`
      (フィルタなしは `len()` と一致)
- [x] HTTP: `GET /collections/{name}/records?limit=&after=&filter=`
- [x] CLI: `hamane scan <db> <collection> [--limit] [--after] [--filter]`
- [x] Python: `col.scan(limit=, after=, filter=)` / `col.count(filter=)`

## 完了条件

- [x] 全件列挙が実データと一致する (削除・上書き後も、`get` の結果と一致)
- [x] `after` + `limit` のページングで**重複も取りこぼしも無い**
- [x] フィルタつき `count` が総当たりの数と一致する
- [x] 大きな collection で `limit` が効く (全件走査していないことを確認)

## 実装メモ

- `Collection::walk` を内部に置き、`scan` / `count` / `delete_by_filter` が
  共有する。各ソース (memtable / セグメント) を id 昇順に **k-way マージ**
  (BinaryHeap) し、`limit` に達したら打ち切る
- **セグメントの行は id 昇順**なので (`SegmentWriter` が id でソートして書く)、
  そのまま順序付きソースとして使える。memtable は HashMap なので id を
  取り出してソートする (フラッシュ閾値ぶんの件数なので安い)
- newest-wins と tombstone の解決は `LiveView::get(id)` に任せた。
  複数ソースに同じ id があってもマージで隣接するので 1 回だけ扱えばよく、
  `get` が None を返したもの (削除済み・shadow された行) は落とす
- 文字列 ID は `_ext_id` メタデータから復元して `RecordId::Str` で返す。
  `after` に文字列を渡した場合は内部 id に解決してから比較する
- `count(None)` は `len()` (O(1))。フィルタありは全件走査になる旨を doc に明記
