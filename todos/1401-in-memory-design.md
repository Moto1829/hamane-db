# 1401: メモリ上セグメントの設計文書

- Status: TODO
- Milestone: M14
- Depends: なし
- Design: docs/design/in-memory.md (このタスクで作る)

## ゴール

`Database::in_memory()` を「永続化しないモード」から「**永続化先が RAM なだけの
モード**」に変えるための設計を書く。

現状の in-memory は `Store.db_dir: Option<PathBuf>` が `None` のとき
フラッシュ経路を丸ごと止める実装で、セグメントが 1 つも作られない。
索引 (HNSW / IVF) と量子化 (SQ8 / PQ / OPQ) は**すべて
`SegmentWriter::write` の中で構築される**ため、in-memory では検索が
memtable の総当たり (Flat) 走査になる。API は `open` と同一なのに
性能特性だけが別物、という状態になっている
(制約は docs/spec/src/limits.md「in-memory モードの制約」)。

## 調査済みの前提 (設計の根拠)

- **読み出しの mmap 依存は `MappedFile` 1 箇所に閉じている**
  (`crates/hamane-storage/src/segment.rs`)。外に見せているのは
  `content() -> &[u8]` / `verify_checksum` / `u64_at` の 3 つだけなので、
  内部を `enum Backing { Mapped(Mmap), Owned(Vec<u8>) }` にすれば
  `Segment` 本体は無改造で済む
- **書き出しは「Vec<u8> を組み立てて `write_file_with_crc` に渡す」構造**
  (同 segment.rs)。つまりファイルに落とす直前のバイト列が既に手元にある
- **フラッシュとコンパクションは同じ `SegmentWriter::write` に集約されている**
  (`store.rs` の `flush_pending` と compaction 経路の 2 箇所)。
  ここを分岐させれば両方の経路を一度に賄える
- **manifest は元々インメモリ状態を持っている** (`state.manifest`)。
  永続化しているのは `manifest.store(&db_dir)` の 1 行だけ

## やること

- [ ] `docs/design/in-memory.md` を新規作成し、以下を決める
  - [ ] **バイト列の抽象**: `Backing` enum の形と、CRC フッタの扱い
        (ファイル経路は `content()` が末尾 4 バイトを除く前提。
        メモリ経路でも同じ表現を保つか、フッタを持たせないか)
  - [ ] **`SegmentWriter` の分割**: 「各ファイルのバイト列を作る」部分と
        「ディレクトリへ落とす」部分の境界。戻り値の型
  - [ ] **`Segment` の生成経路**: `Segment::open(dir, seg_id)` と対になる
        メモリ版コンストラクタの形。`load_pq` / `load_ivf` / `load_ivfpq` /
        opq が `&Path` を取っているのでバイト列を取る形に揃える
  - [ ] **Store 側の分岐点**: `rotate` (WAL を作らない) /
        `flush_pending` (manifest を書かない) / compaction /
        セグメント削除 (メモリ版は Arc の drop のみ) をどう場合分けするか
  - [ ] **`StoreOptions` の受け渡し**: `Store::in_memory_with_options` /
        `Database::in_memory_with_options` の API 形
  - [ ] **アラインメント**: `Segment::open` は vectors.bin の
        `VECTORS_HEADER_LEN` (= 64) 以降が 4 バイト境界にあることを実行時に
        検査し、外れていれば `corrupted` を返す。ヘッダ長は 4 の倍数なので
        判定は**バッファ先頭のアラインメント次第**になるが、`Vec<u8>` が
        要求するアラインメントは 1 で、4 以上は言語仕様上は保証されない
        (現実のアロケータでは通常 8〜16)。オーバーアラインした確保
        (`Vec<u64>` 経由など) を用意するか、検査を通せる形を決める。
        **設計上の要注意点**
  - [ ] **非ゴール**: 永続化との相互変換 (メモリ DB の backup、
        ディスク DB のメモリ読み込み) は対象外とする
- [ ] メモリ使用量の見積もりを書く (f32 の生ベクトルに加えて HNSW グラフ・
      量子化コードが載るので、現状の memtable のみより増える構成がある)

## 完了条件

- [ ] docs/design/in-memory.md がレビュー可能な状態
- [ ] 1402〜1405 のタスクが設計に沿って書き直されている
