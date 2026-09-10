# 1404: in-memory のフラッシュ・コンパクション経路

- Status: TODO
- Milestone: M14
- Depends: 1402, 1403
- Design: docs/design/in-memory.md

## ゴール

`db_dir` が `None` のときもフラッシュとコンパクションを走らせ、
メモリ上セグメントを作る。これで in-memory でも HNSW・量子化が効くようになる。

## 現状の分岐 (すべて `store.rs`)

| 箇所 | 現状 |
|---|---|
| `maybe_flush` | `db_dir.is_none()` で即 return |
| `rotate` | `db_dir` が無ければ `Ok(false)` (新 WAL を作れないため) |
| `flush_pending` | `db_dir` が無ければ `Ok(())` |
| `flush()` / `compact()` | メンテナンススレッドが無いので no-op |
| `Store::in_memory()` | `StoreOptions::default()` 固定、`maint_tx` なし |

## やること

- [ ] `Store::in_memory_with_options(StoreOptions)` と
      `Database::in_memory_with_options` を追加する。
      `validate()` は open 経路と同じものを通す
- [ ] in-memory でもメンテナンススレッドを起動する。
      フラッシュ判定 (`flush_threshold_bytes`) とコンパクション判定
      (`compaction_threshold`) はディスク版と同じ条件で効かせる
- [ ] `rotate`: WAL を作らずに memtable を pending へ移す経路を用意する
      (WAL が無いだけで、切り替え自体はディスク版と同じ)
- [ ] `flush_pending`: 1403 の `build` を使ってメモリ上セグメントを作り、
      `manifest.store(&db_dir)` を**呼ばない**。`state.manifest` の
      世代・`next_seg_id` の更新はディスク版と同じに保つ
- [ ] コンパクション: 統合後の旧セグメントはファイル削除ではなく
      `Arc` の drop に任せる
- [ ] `backup()` は引き続き `InvalidConfig` (メモリ DB のバックアップは非ゴール)
- [ ] `SyncPolicy` は in-memory では意味を持たない。`validate` で弾くか
      無視するかを 1401 の設計に合わせる

## 完了条件

- [ ] `Database::in_memory_with_options` に HNSW / SQ8 / PQ / OPQ / IVF /
      IVF-PQ の各構成を渡して、**フラッシュ後に索引経由で検索できる**こと
      (`tests/matrix.rs` の構成表を in-memory でも回すのが早い)
- [ ] in-memory で recall@10 が同構成のディスク版と一致すること
- [ ] `flush()` / `compact()` が no-op でなく実際に効くこと
      (セグメント数・`live_len` で確認)
- [ ] 既存の `in_memory_mode_works_without_files` に相当する
      「フラッシュ前でも検索できる」性質が保たれていること
- [ ] `cargo test --workspace` と clippy が green

## 備考

**既存の `Database::in_memory()` の挙動が変わる**点に注意。既定の
`flush_threshold_bytes` (64 MiB) を超えるまでフラッシュされないので
小規模な使い方では今までどおりだが、大量投入時に索引が作られるようになる。
非互換ではないが、リリースノートに書く価値がある。
