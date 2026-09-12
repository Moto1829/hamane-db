# CLI リファレンス

`hamane` コマンド (crates/hamane-cli)。出力はすべて JSON で、
エラー時は stderr にメッセージを出して終了コード 1 を返します。

## コマンド一覧

### create — Collection 作成

```sh
hamane create <DB_DIR> <COLLECTION> --dim <N> [--metric l2|cosine|dot]
```

`--metric` の既定は `cosine`。

### insert — JSONL の一括挿入

```sh
cat records.jsonl | hamane insert <DB_DIR> <COLLECTION>
```

stdin から 1 行 1 レコードの JSON を読みます (1000 件ごとにバッチ書き込み):

```json
{"id": 1, "vector": [0.1, 0.2, 0.3], "meta": {"lang": "ja", "year": 2026}}
```

- `id`: 非負整数 (必須)
- `vector`: 数値配列 (必須、Collection の dim と一致)
- `meta`: 文字列 / 数値 (整数は Int、小数は Float) / 真偽値のオブジェクト (任意)

### search — 近傍検索

```sh
hamane search <DB_DIR> <COLLECTION> \
    --vector '[0.1,0.2,0.3]' \
    [--k 10] [--ef 64] [--nprobe 8] [--threshold 0.2] \
    [--filter '<FILTER_JSON>'] [--pretty]
```

| オプション | 既定 | 意味 |
|---|---|---|
| `--k` | 10 | 取得件数 |
| `--ef` | DB 設定 | HNSW の探索幅。大きいほど高精度・低速 |
| `--nprobe` | DB 設定 | IVF / IVF-PQ で走査するクラスタ数 |
| `--threshold` | なし | スコア閾値 (l2 は距離がこれ以下、cosine/dot はスコアがこれ以上) |
| `--filter` | なし | メタデータ条件 (下記の JSON 表現) |
| `--pretty` | off | 人間向けに整形して出力 |

出力:

```json
{"hits": [{"id": 1, "ext_id": null, "score": 0.98, "meta": {"lang": "ja"}}]}
```

`score` は metric に応じた値です (L2 は距離、Cosine は類似度、Dot は内積)。
文字列 ID のレコードは `ext_id` に元の文字列が入ります。

### info — 状態表示

```sh
hamane info <DB_DIR>
# {"collections":[{"name":"docs","dim":3,"metric":"Cosine","len":100}]}
```

### flush / compact — メンテナンス

```sh
hamane flush <DB_DIR>     # memtable をセグメント化
hamane compact <DB_DIR>   # flush + セグメント統合 (ディスク回収)
hamane backup <DB_DIR> <DEST_DIR>   # 一貫性のあるバックアップ (DEST は空)
```

## フィルタの JSON 表現

Rust API の `Filter` と 1:1 に対応します。

| JSON | 意味 |
|---|---|
| `{"eq": ["lang", "ja"]}` | `lang == "ja"` |
| `{"in": ["lang", ["ja", "en"]]}` | いずれかに一致 |
| `{"gt": ["year", 2000]}` | `year > 2000` (`gte` / `lt` / `lte` も同様) |
| `{"and": [f1, f2, ...]}` | 論理積 |
| `{"or": [f1, f2, ...]}` | 論理和 |
| `{"not": f}` | 否定 |

例 — 「2000 年より後で、言語が en ではない」:

```sh
--filter '{"and": [{"gt": ["year", 2000]}, {"not": {"eq": ["lang", "en"]}}]}'
```

## 使用例 (一連の流れ)

```sh
hamane create ./db docs --dim 3 --metric l2

for i in $(seq 0 99); do
  echo "{\"id\": $i, \"vector\": [$i, 0, 0], \"meta\": {\"even\": $((i % 2 == 0))}}"
done | hamane insert ./db docs

hamane search ./db docs --vector '[5,0,0]' --k 3 \
    --filter '{"eq": ["even", true]}' --pretty

hamane info ./db
```

## レシピ

### CSV から投入する

`jq` で JSONL に変換して `insert` に流し込みます
(1 行 = `{"id":..,"vector":[..],"meta":{..}}`)。

```sh
# id,lang,v1,v2,v3 の CSV を JSONL にする
tail -n +2 data.csv | jq -R -c 'split(",") |
    {id: (.[0]|tonumber),
     vector: [.[2],.[3],.[4] | tonumber],
     meta: {lang: .[1]}}' \
  | hamane insert ./db docs
```

### 埋め込みモデルの出力を入れる

多くのモデルは JSON 配列を返すので、そのまま組み立てられます。

```sh
for f in docs/*.txt; do
  vec=$(your-embedder "$f")          # 例: [0.12, -0.03, ...]
  jq -n -c --arg id "$f" --argjson v "$vec" \
    '{id: $id, vector: $v, meta: {path: $id}}'
done | hamane insert ./db docs
hamane flush ./db
```

### 大量投入のあとに整える

`insert` は 1000 件ごとにまとめて書き込みます。投入後に
`flush` (セグメント化 + 索引構築) と `compact` (統合) を呼ぶと
読み取りに最適な形になります。

```sh
cat records.jsonl | hamane insert ./db docs
hamane flush ./db
hamane compact ./db
hamane info ./db     # セグメント構成を確認
```

### バックアップを取る

```sh
hamane backup ./db /backup/hamane-$(date +%Y%m%d)
# 復元は「そのディレクトリを開くだけ」
hamane info /backup/hamane-20260910
```

### 検索結果を後段に渡す

```sh
# id だけ取り出す
hamane search ./db docs --vector "$q" --k 5 | jq -r '.hits[].id'

# 文字列 ID (ext_id) を使う
hamane search ./db docs --vector "$q" --k 5 | jq -r '.hits[] | .ext_id // .id'
```
