# 205: セグメント読み込み (mmap)

- Status: DONE (2026-07-12)
- Milestone: M2
- Depends: 204
- Design: docs/design/storage.md §3

## ゴール

セグメントを mmap で開き、コピーなしで距離計算・点参照できるビューを提供する。

## やること

- [ ] `Segment::open(dir, seg_id) -> Result<Segment>` (memmap2 で 4 ファイルを map)
  - ヘッダ検証 (magic / version / count 整合)。`verify_checksums()` は別メソッド
- [ ] アクセサ:
  - `vector(row) -> &[f32]` (アラインメント検証つき。ずれていたら Corrupted)
  - `id(row) -> Id` / `row_of(id) -> Option<u32>` (索引の二分探索)
  - `metadata(row) -> Result<Metadata>` (blob をその場でデコード)
  - `is_tombstoned(id) -> bool` (昇順列の二分探索)
  - `iter()` — search_flat に渡せる `(Id, &[f32], &Metadata)` 走査
    (Metadata はデコード済みを都度返すため所有型を工夫: 行ごとにデコードして
    クロージャに渡す visitor 形式でも可)
- [ ] `Segment` は `Send + Sync`。`Arc<Segment>` で共有する前提の設計

## 完了条件

- writer で書いたセグメントを開き、全行が元の memtable と一致する
- 全アクセサのラウンドトリップテスト + 破損ファイルで Corrupted になるテスト
- `search_flat` にセグメントを渡して memtable 時と同じ検索結果が得られる
