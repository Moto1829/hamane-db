# 1701: メタデータのみの更新 API

- Status: TODO
- Milestone: M17
- Depends: 1601 (列挙), 1602 (一括削除)
- Design: —

## ゴール

**タグを 1 つ変えるためにベクトルごと入れ直す**のをやめる。

現状メタデータを変える手段は `upsert` しかない。呼び出し側は

1. `get(id)` でレコードを取り出し (ベクトル全体のコピーが発生)
2. メタデータを組み立て直し
3. ベクトルを付けて `upsert`

をやる必要がある。「公開フラグを立てる」「テナントを付け替える」のような
運用操作でこれは重い。条件による一括更新に至っては、1602 で列挙できるように
なったとはいえ全部自前で回すことになる。

## やること

- [ ] `Collection::update_meta(id) -> MetaUpdate` (単体)
  - `.set(key, value)` / `.remove(key)` を積んで `.run() -> Result<usize>`
    (更新できたら 1、対象が無ければ 0)
- [ ] `Collection::update_meta_by_filter(&Filter) -> MetaUpdate` (一括)
  - `.run() -> Result<usize>` は更新件数
- [ ] **マージ意味論**にする: `set` したキーだけ上書き、触れていないキーは保持。
      消したいものは `remove` で明示する
- [ ] `_ext_id` (文字列 ID の対応表) を `set` / `remove` させない。
      触ろうとしたら `InvalidConfig` を返す
- [ ] 一括更新は**カーソル (`after`) で前へ進める**。
      1602 のように「毎回先頭から走査」すると、更新後もフィルタに一致し続ける
      場合に無限ループになる
- [ ] HTTP: `PATCH /collections/{name}/records/{id}` と
      `PATCH /collections/{name}/records` (body に filter)
- [ ] CLI: `hamane update-meta <db> <collection> (--id <ID> | --filter <JSON>)
      [--set k=v]... [--remove k]...`
- [ ] Python: `col.update_meta(id, set=..., remove=...)` /
      `col.update_meta_by_filter(filter, set=..., remove=...)`

## 完了条件

- [ ] 触れていないキーとベクトルが保たれる
- [ ] `remove` したキーが消える。存在しないキーの `remove` はエラーにしない
- [ ] 一括更新が「更新後もフィルタに一致する」条件でも停止する (無限ループ防止)
- [ ] 更新が検索結果のメタデータ・`scan`・`get` の全経路に反映される
- [ ] フラッシュを跨いでも保たれる (セグメント上のレコードも更新できる)

## スコープ外 (次の候補)

v0 は**内部で `upsert` に落とす**。つまりベクトルは書き直され、WAL にも
レコード全体が載る。メタデータだけの WAL レコード型 (`UpdateMeta`) を足せば
WAL 量は dim に比例しなくなる (dim=768 なら 1 件 3 KB → 数十バイト) が、
フォーマット変更とリプレイ対応が要るので別タスクにする。
