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
    [--k 10] [--ef 64] [--filter '<FILTER_JSON>'] [--pretty]
```

出力:

```json
{"hits": [{"id": 1, "score": 0.98, "meta": {"lang": "ja"}}]}
```

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
