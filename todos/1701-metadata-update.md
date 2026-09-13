# 1701: メタデータのみの更新 API

- Status: DONE (2026-09-13)
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

- [x] `Collection::update_meta(id) -> MetaUpdate` (単体)
  - `.set(key, value)` / `.remove(key)` を積んで `.run() -> Result<usize>`
    (更新できたら 1、対象が無ければ 0)
- [x] `Collection::update_meta_by_filter(&Filter) -> MetaUpdate` (一括)
  - `.run() -> Result<usize>` は更新件数
- [x] **マージ意味論**にする: `set` したキーだけ上書き、触れていないキーは保持。
      消したいものは `remove` で明示する
- [x] `_ext_id` (文字列 ID の対応表) を `set` / `remove` させない。
      触ろうとしたら `InvalidConfig` を返す
- [x] 一括更新は**カーソル (`after`) で前へ進める**。
      1602 のように「毎回先頭から走査」すると、更新後もフィルタに一致し続ける
      場合に無限ループになる
- [x] HTTP: `PATCH /collections/{name}/records/{id}` と
      `PATCH /collections/{name}/records` (body に filter)
- [x] CLI: `hamane update-meta <db> <collection> (--id <ID> | --filter <JSON>)
      [--set k=v]... [--remove k]...`
- [x] Python: `col.update_meta(id, set=..., remove=...)` /
      `col.update_meta_by_filter(filter, set=..., remove=...)`

## 完了条件

- [x] 触れていないキーとベクトルが保たれる
- [x] `remove` したキーが消える。存在しないキーの `remove` はエラーにしない
- [x] 一括更新が「更新後もフィルタに一致する」条件でも停止する (無限ループ防止)
- [x] 更新が検索結果のメタデータ・`scan`・`get` の全経路に反映される
- [x] フラッシュを跨いでも保たれる (セグメント上のレコードも更新できる)

## スコープ外 (次の候補)

v0 は**内部で `upsert` に落とす**。つまりベクトルは書き直され、WAL にも
レコード全体が載る。メタデータだけの WAL レコード型 (`UpdateMeta`) を足せば
WAL 量は dim に比例しなくなる (dim=768 なら 1 件 3 KB → 数十バイト) が、
フォーマット変更とリプレイ対応が要るので別タスクにする。

## 実装メモ

- `MetaUpdate` ビルダー 1 つで単体と一括の両方を扱う (`UpdateTarget` で分岐)。
  戻り値はどちらも更新件数 (単体は 1 か 0)
- **一括はカーソル (`after`) で前へ進める**のが肝。1602 の削除と同じ
  「毎回先頭から走査」にすると、`tenant=old` のまま別のキーを足すような
  「更新後も一致し続ける」条件で同じレコードを拾い続けて終わらない
- `_ext_id` の変更は `InvalidConfig` で拒否する。ここを書き換えられると
  文字列 ID の解決が壊れる
- CLI の `--set key=value` は値を JSON として解釈し、読めなければ文字列にする
  (`year=2026` → Int、`public=true` → Bool、`lang=ja` → Str)

### 副次的に見つかった不具合

`hamane` クレートの `lib.rs` で **`ScanBuilder` (M16) が再エクスポートされて
いなかった**。`col.scan()` はメソッドチェーンで使えるので気づかなかったが、
戻り値の型を利用者が名前で書けない状態だった。`MetaUpdate` と合わせて修正した。
