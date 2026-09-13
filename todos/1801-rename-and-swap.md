# 1801: collection のリネームと名前の入れ替え

- Status: DONE (2026-09-13)
- Milestone: M18
- Depends: 207 (復旧), 206 (manifest)
- Design: —

## ゴール

**索引を作り直したときに、無停止で切り替えられるようにする。**

埋め込みモデルを差し替えた / 量子化構成を変えた、といった理由で collection を
作り直すのは日常的だが、現状 collection の名前は作成時に決めたら変えられない。
利用者は「新しい名前を全アプリに反映する」しかなく、切り替えの瞬間に
参照先がばらつく。

定石は「読み手は固定の名前を使い、裏で実体を差し替える」こと。そのための
最小の道具として **リネーム**と、**2 つの名前を原子的に入れ替える** swap を用意する。

```text
docs_v2 を作って投入 → swap(docs, docs_v2) → 読み手は "docs" のまま新実体を見る
                                            → 旧実体は "docs_v2" として残るので検証後に drop
```

## やること

- [x] `WalRecord::RenameCollection { collection_id, new_name }` (タグ 5)
- [x] `WalRecord::SwapCollectionNames { a, b }` (タグ 6)
      **swap を「2 回のリネーム」で表現しない**。WAL が途中で切れると
      名前が重複した壊れた状態になるので、1 レコードで原子的に適用する
- [x] `Store::rename_collection(from, to)` / `swap_collection_names(a, b)`
- [x] `Database::rename_collection(from, to)` / `swap_collections(a, b)`
- [x] 検証: リネーム先が既存なら `CollectionExists`、対象が無ければ
      `CollectionNotFound`。同名へのリネームは何もしない
- [x] リプレイ (WAL) と manifest の両方で名前が復元されること
- [x] HTTP: `POST /collections/{name}/rename` (body: `{"to": ...}`)、
      `POST /admin/collections/swap` (body: `{"a": ..., "b": ...}`)
- [x] CLI: `hamane rename <db> <from> <to>` / `hamane swap <db> <a> <b>`
- [x] Python: `db.rename_collection(from, to)` / `db.swap_collections(a, b)`
- [x] `docs/spec/src/file-format.md` の WAL レコード表を更新 (タグ 5, 6)

## 完了条件

- [x] リネーム後、新しい名前で開けて中身が同一 (件数・検索結果)
- [x] リネーム / swap がプロセス再起動 (WAL リプレイ) をまたいで保たれる
- [x] フラッシュを挟んで manifest に載った後も保たれる
- [x] swap がクラッシュ耐性テストを壊さない (名前の重複が起きない)
- [x] 「無停止切り替え」のシナリオが E2E テストで通る

## メモ

WAL のタグ追加は**新しいバイナリが古い WAL を読める**方向の互換性は保つ
(既存タグの意味を変えない)。古いバイナリが新しい WAL を読むことは想定しない
(ダウングレード保証は元々していない)。

## 実装メモ

- WAL にタグ 5 (`RenameCollection`) と 6 (`SwapCollectionNames`) を追加した。
  既存タグの意味は変えていないので、新しいバイナリは古い WAL をそのまま読める
- **swap を 1 レコードにしたのが肝**。2 回のリネームで表現すると、WAL が
  1 本目と 2 本目の間で切れたときに 2 つの collection が同じ名前を持つ
  (`state.names` が片方を上書きして、もう片方が引けなくなる) 状態になる
- 名前の解決は `state.names` (name → collection_id) と
  `CollectionState.name` の 2 箇所にあるので、apply 時に両方を更新する
- 既に取得済みの `Collection` ハンドルは古い名前を持ったままだが、
  内部 ID で動くので読み書きは問題ない (doc に明記)
