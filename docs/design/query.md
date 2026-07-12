# 詳細設計: クエリエンジンと並行性 (hamane)

DESIGN.md §5–6 の詳細化。検索の実行計画、スナップショット分離、
公開 API の最終形を規定する。

- Status: Draft v0.1 (2026-07-12)
- 対応タスク: todos/209, 305, 306

---

## 1. Collection 内部構造 (M2 以降)

```rust
pub struct Collection {
    name: String,
    config: CollectionConfig,
    inner: RwLock<CollectionInner>,   // 書き込みと世代切り替えを直列化
}

struct CollectionInner {
    memtable: Memtable,
    segments: Vec<Arc<Segment>>,      // seg_id 降順
    wal: WalWriter,
}
```

- **書き込み** (upsert/delete): `inner` の write ロック内で WAL append+sync →
  memtable 反映。フラッシュ閾値を超えたらフラッシュ (storage.md §6)
- **読み取り**: write ロックを短時間だけ取って `LiveView` (memtable の
  スナップショット + segments の Arc clone) を作り、ロックを放してから検索する。
  memtable スナップショットは v0 ではデータの clone で作る
  (フラッシュ閾値 64 MiB が clone コストの上限。ロックフリー化は M4 で検討)

## 2. 検索の実行フロー

```
SearchBuilder::run()
  1. クエリ検証 + 正規化 (prepare_vector)
  2. LiveView 取得
  3. ソースごとにプラン選択:
       memtable                → Flat (+ フィルタ逐次判定)
       segment + hnsw.bin なし → Flat
       segment + hnsw.bin あり → フィルタなし: HNSW(ef)
                                 フィルタあり: pre/post 選択 (index.md §5)
  4. 各ソースを走査し、live でない行 (より新しいソースに上書き・削除された行) は
     走査時に LiveView::is_live で除外して上位 k を収集
  5. 距離キーで k 件にマージ → score 変換 → メタデータ付与
```

- 4 のソース並列化 (rayon) は M4。v0 は逐次
- live 判定を「収集後の除外」でなく「走査時の除外」で行うのは、shadowed な行が
  ソース内 top-k を占有して真の近傍を押し出すのを防ぐため。これにより結果は
  常に正確な top-k になる (M3 の HNSW post-filter では oversampling で近似する)

## 3. 公開 API の最終形 (M3 完了時点)

```rust
let db = Database::open("path/to/db")?;          // 永続化 (M2)
let db = Database::in_memory();                   // 従来どおり

let col = db.create_collection("docs", CollectionConfig {
    dim: 768,
    metric: Metric::Cosine,
})?;

col.upsert(record)?;
col.upsert_batch(records)?;      // WAL sync を 1 回に集約 (M2)
col.delete(id)?;
col.flush()?;                    // 明示フラッシュ (M2)

let hits = col.search(&q)
    .k(10)
    .filter(Filter::eq("lang", "ja"))
    .ef(128)                     // HNSW の ef_search 上書き (M3)
    .run()?;
```

- `Database::open` と `in_memory` は同じ型を返す。in_memory は Store を持たず
  従来の HashMap 実装 (または tempdir) — 実装単純化のため **memtable のみで
  フラッシュ無効の Store** として統一する
- `CollectionConfig` に `flush_threshold_bytes`, `hnsw: HnswParams`,
  `sync: SyncPolicy` を追加 (すべて既定値あり、`Default` 実装を維持)

## 4. スレッド安全性の保証

- `Database`, `Collection` は `Send + Sync`。検索は並行、書き込みは直列
- 検索中のフラッシュ/コンパクションはセグメント Arc の参照カウントで安全
  (ファイル削除は最後の Arc drop 後。Drop impl で削除予約を処理)
- ロック毒化 (poisoned lock) は `expect` で即 panic (書き込み中 panic 後の
  継続利用は未定義として v0 では扱わない)

## 5. エラーセマンティクス

| 状況 | 挙動 |
|---|---|
| WAL 書き込み失敗 (I/O) | エラーを返し、memtable 未反映。Collection は使用継続可 |
| フラッシュ失敗 | エラーを返す。WAL は残っているため再試行・再起動で復元可 |
| open 時の CRC 不一致 (manifest) | `HamaneError::Corrupted` で開けない |
| open 時の WAL 末尾破損 | 正常 (そこまでをリプレイ)。storage.md §2 |

`HamaneError` に追加: `Corrupted(String)`, `WalIo(std::io::Error)` は既存 `Io` に統合。
