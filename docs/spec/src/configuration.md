# 設定リファレンス

## StoreOptions

`Database::open_with_options(path, options)` で指定します。
実行時オプションであり、DB には**永続化されません** (open のたびに指定)。

```rust
use hamane::{Database, StoreOptions, SyncPolicy, HnswParams};

let db = Database::open_with_options("./mydb", StoreOptions {
    sync: SyncPolicy::Always,
    flush_threshold_bytes: 64 * 1024 * 1024,
    hnsw: HnswParams::default(),
    hnsw_min_rows: 1024,
    compaction_threshold: 4,
    sq8: false,
    search_threads: 0,
})?;
```

不正な値 (`dim == 0`、`ef == 0`、`compaction_threshold < 2` 等) は
open 時に `InvalidConfig` エラーになります。

| フィールド | 既定値 | 説明 |
|---|---|---|
| `sync` | `Always` | WAL の fsync ポリシー (下記) |
| `flush_threshold_bytes` | 64 MiB | memtable がこのバイト数を超えたら自動フラッシュ |
| `hnsw` | 下表 | フラッシュ時に構築する HNSW のパラメータ |
| `hnsw_min_rows` | 1024 | この行数未満のセグメントは HNSW を作らない (Flat で検索) |
| `compaction_threshold` | 4 | セグメント数がこの値以上で自動コンパクション |
| `sq8` | false | SQ8 量子化 ([検索](search.md#sq8-量子化による高速化) 参照) |
| `search_threads` | 0 (自動 = 論理コア数) | セグメント並列検索の並列度。1 で逐次。プールは Database 全体で共有され、初回の複数セグメント検索まで worker は起動しない |

## SyncPolicy

| 値 | 挙動 | 耐久性 |
|---|---|---|
| `Always` | 書き込み (バッチ) ごとに fsync | Ok = 永続 (既定) |
| `Batch` | 並行する書き込みの fsync を 1 回に相乗り (group commit) | Ok = 永続。並行書き込みで Always より高スループット |
| `EveryN(n)` | n 回に 1 回 fsync | クラッシュで直近最大 n−1 件を失い得る |

`Batch` は fsync 完了まで呼び出し元に Ok を返さないため耐久性は Always と
同等です。複数スレッドから同時に書き込む場合に効果があります
(単一スレッドでは Always と同じ)。

## HnswParams

| フィールド | 既定値 | 説明 |
|---|---|---|
| `m` | 16 | 層 1 以上の最大接続数。大きいと精度↑・メモリ↑ |
| `m0` | 32 | 層 0 の最大接続数 (慣例的に 2m) |
| `ef_construction` | 200 | 構築時の候補リスト幅。大きいと精度↑・構築時間↑ |
| `ef_search` | 64 | 検索時の候補リスト幅の既定値。クエリごとに `.ef(n)` で上書き可 |
| `seed` | 0 | レベル抽選の乱数 seed。実際の構築ではセグメント ID で上書きされる |
| `extend_candidates` | true | 構築時の候補拡張。クラスタ化したデータの再現率を保つ。自然データでは false で構築が約 20% 速くなる |
| `build_threads` | 0 (自動 = 全コア) | 構築スレッド数。**2 以上ではグラフが非決定** (品質は保たれる)。1 で決定的構築 |

パラメータと再現率・速度の実測は [ベンチマーク](benchmarks.md) を参照。

## CollectionConfig

Collection 作成時に指定し、**以後変更できません**。
dim と metric は manifest に永続化され、再 open 後も保持されます。

| フィールド | 既定値 | 説明 |
|---|---|---|
| `dim` | — (必須, > 0) | ベクトルの次元数 |
| `metric` | `Cosine` | 距離関数 ([データモデル](data-model.md#metric-距離関数)) |

## チューニングの指針

- **書き込みスループット優先**: `upsert_batch` を使う。さらに必要なら
  `SyncPolicy::EveryN` (耐久性とのトレードオフを理解した上で)
- **検索の再現率優先**: クエリ側で `.ef(128〜256)` を上げるのが最も簡単。
  恒久的に上げるなら `StoreOptions.hnsw.ef_search`
- **再起動を速く**: 定期的に `flush()` して WAL を短く保つ
- **ディスク回収**: 削除・上書きが多いワークロードでは `compact()` を
  明示的に呼ぶか、`compaction_threshold` を下げる
