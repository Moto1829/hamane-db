# データモデル

## 階層

```text
Database (= 1 ディレクトリ)
 └── Collection (名前つき。次元数と距離関数を固定)
      └── Record (id + ベクトル + メタデータ)
```

## Database

永続化の単位。1 つのディレクトリに対応します。

| メソッド | 説明 |
|---|---|
| `Database::open(path)` | ディレクトリを開く。なければ初期化。クラッシュ後は自動復旧 |
| `Database::open_with_options(path, StoreOptions)` | 設定つきで開く |
| `Database::in_memory()` | 永続化なし。API は同一 |
| `create_collection(name, config)` | Collection 作成。同名が存在すればエラー |
| `collection(name)` | 既存 Collection のハンドル取得 |
| `drop_collection(name)` | Collection 削除 (データも削除される) |
| `collection_names()` | 名前一覧 (ソート済み) |
| `flush()` | 全 Collection の memtable をセグメント化 |
| `compact()` | セグメント統合 (上書き・削除の物理適用) |

**スレッド安全性**: `Database` / `Collection` は `Send + Sync` です。
検索は並行に実行でき、書き込みは内部で直列化されます。
1 つの DB ディレクトリを開けるプロセスは 1 つだけです
([制限事項](limits.md) 参照)。

## Collection

次元数 `dim` と距離関数 `metric` を作成時に固定した、レコードの集合。

```rust
pub struct CollectionConfig {
    pub dim: usize,     // 必須 (> 0)
    pub metric: Metric, // 既定: Cosine
}
```

| メソッド | 説明 |
|---|---|
| `upsert(record)` | 挿入。同 id は置き換え |
| `upsert_batch(records)` | 一括挿入 (WAL 同期 1 回) |
| `delete(id) -> bool` | 削除。呼び出し前に存在していたら true (判定と削除は原子的) |
| `get(id) -> Option<Record>` | 点参照 |
| `search(&query)` | 検索ビルダー ([検索](search.md)) |
| `len()` / `is_empty()` | 有効レコード数 (O(1)。書き込み時に差分維持) |
| `segment_stats()` | セグメント構成の要約 (監視・デバッグ用) |
| `flush()` | DB 全体のフラッシュ (Database::flush と同じ) |

## Record と ID

```rust
let rec = Record::new(42u64, vec![0.1, 0.2, /* dim 個 */])
    .with_meta("lang", "ja")
    .with_meta("year", 2026)
    .with_meta("score", 0.5)
    .with_meta("public", true);

// 文字列 ID も使える (UUID 等)
let rec = Record::new("doc-550e8400", vec![0.1, 0.2]);
col.get("doc-550e8400");
col.delete("doc-550e8400");
```

- **id**: `u64` または文字列 (`RecordId`)。Collection 内で一意。
  同じ id への upsert は置き換え
- **文字列 ID の仕組み**: collection ごとの辞書で内部 u64
  (`EXT_ID_BASE` = 2^63 以降を採番) に対応づけられ、予約メタデータキー
  `_ext_id` として永続化される。再 open 時に辞書は自動再構築される。
  `SearchHit::ext_id()` で検索結果から文字列 ID を取得できる
- **u64 と文字列の混在**: 可能だが、u64 側は 2^63 未満を使うこと
  (文字列 ID の採番領域との衝突を避けるため)
- **vector**: `Vec<f32>`。長さは Collection の `dim` と一致が必須。
  NaN / 無限大を含むとエラー
- **metadata**: 文字列キー → スカラー値のマップ (`_ext_id` キーは予約)。
  値の型は 4 種:

| MetaValue | Rust 型 | `with_meta` に渡せる型 |
|---|---|---|
| `Str` | `String` | `&str`, `String` |
| `Int` | `i64` | `i64`, `i32` |
| `Float` | `f64` | `f64` |
| `Bool` | `bool` | `bool` |

## Metric (距離関数)

| Metric | score の意味 | 並び順 | 挿入時の処理 |
|---|---|---|---|
| `L2` | ユークリッド距離 | 小さいほど近い | なし |
| `Cosine` | コサイン類似度 [-1, 1] | 大きいほど近い | **L2 正規化される** |
| `Dot` | 内積 | 大きいほど近い | なし |

`Cosine` に関する仕様:

- ベクトルは挿入時に L2 ノルム 1 に正規化されます。`get()` が返すのは
  **正規化後**のベクトルです
- ゼロベクトル (ノルム 0) は挿入できません (`InvalidVector` エラー)
- 検索クエリも同様に正規化されます

## メタデータフィルタ

`Filter` はメタデータに対する述語で、検索を絞り込みます。

```rust
Filter::eq("lang", "ja")                 // 等価
Filter::is_in("lang", ["ja", "en"])      // いずれかに一致
Filter::gt("year", 2000)                 // >   (gte / lt / lte も同様)
Filter::and([f1, f2])                    // 論理積
Filter::or([f1, f2])                     // 論理和
Filter::not(f)                           // 否定
```

評価規則 (保証):

- **キーが存在しない場合**、比較系の条件 (`eq` / `is_in` / 大小比較) は
  すべて**不成立**。`Filter::not` で包んだ場合のみ成立する
- 数値比較 (`gt` 等) と `eq` は **Int と Float を相互に比較可能**
  (`Filter::gt("year", 2025.5)` は `Int(2026)` に一致)
- 数値以外の型への大小比較は不成立
- `and([])` は全件成立、`or([])` は全件不成立
