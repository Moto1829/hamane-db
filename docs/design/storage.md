# 詳細設計: ストレージ層 (hamane-storage)

DESIGN.md §3 の詳細化。WAL・セグメント・manifest のオンディスクフォーマットと
復旧手順を規定する。

- Status: Draft v0.1 (2026-07-12)
- 対応タスク: todos/201〜211

---

## 1. 共通規約

- すべてリトルエンディアン
- 文字列は `len: u32` + UTF-8 バイト列
- チェックサムは CRC32C (Castagnoli)。`crc32c` クレートを使用。
  セグメント・manifest の footer CRC は「ファイル先頭から footer 直前まで」の
  全バイトに対して計算する
- 各ファイルは 8 バイトの magic で始まる: `b"HAMANE"` + 用途 1 文字 + version 1 バイト
  - WAL: `b"HAMANEW\x01"` / vectors: `b"HAMANEV\x01"` / ids: `b"HAMANEI\x01"`
  - meta: `b"HAMANEM\x01"` / tombstones: `b"HAMANET\x01"` / manifest: `b"HAMANEF\x01"`
  - HNSW: `b"HAMANEH\x01"` (index 層で定義)
- magic 不一致・未知 version は即エラー (前方互換は将来 version bump で対応)

### MetaValue のバイナリ表現

```
tag u8: 0=Str, 1=Int, 2=Float, 3=Bool
Str:   tag + string
Int:   tag + i64
Float: tag + f64
Bool:  tag + u8 (0/1)
```

Metadata 全体: `count: u32` + count 回の `(key: string, value: MetaValue)`。
BTreeMap 由来なのでキー昇順で書かれる (決定的なバイト列になる)。

---

## 2. WAL

### ファイル

`<db_dir>/wal/<seq:020>.wal`。seq は単調増加の u64。
アクティブな WAL は常に 1 本。フラッシュ完了で新しい seq に切り替え、
旧 WAL は manifest コミット後に削除する。

### レコードフレーム

```
+------------+----------+---------+------------------+
| crc32c u32 | len u32  | type u8 | payload (len-1)  |
+------------+----------+---------+------------------+
```

- `len` = type + payload のバイト数
- `crc32c` は type + payload に対して計算

### レコード種別

| type | 名前 | payload |
|---|---|---|
| 1 | Upsert | `collection_id u32, id u64, dim u32, vector f32×dim, metadata` |
| 2 | Delete | `collection_id u32, id u64` |
| 3 | CreateCollection | `collection_id u32, name string, dim u32, metric u8 (0=L2,1=Cosine,2=Dot)` |
| 4 | DropCollection | `collection_id u32` |

collection_id は manifest が採番する内部 ID (名前変更に強くするため)。
Upsert の vector は正規化済み (Cosine の場合)。検証は WAL 書き込み前に完了している。

### 書き込みと fsync

- `WalWriter::append(record) -> Result<()>`: フレームをバッファに追記
- `WalWriter::sync()`: `File::sync_data`。v0 の既定は**書き込みごとに sync**
  (`SyncPolicy::Always`)。`SyncPolicy::EveryN(n)` / `Batch(interval)` は設定で選択可
- 呼び出し元 (Collection) は sync 完了後に memtable へ反映する

### リプレイ

先頭から順にフレームを読む。以下のいずれかで**そこで停止し、ファイルをその位置で
切り詰めて**正常終了とする (クラッシュによる部分書き込みは末尾にしか現れない前提):

- EOF / len 分のバイトが足りない (部分書き込み)
- CRC 不一致

途中のフレームで CRC 不一致が起き、その後に正常なフレームが続く場合はディスク破損
なのでエラーにする…ことはせず、v0 では単純に停止する (既知の制限として記録)。

---

## 3. セグメント

`<db_dir>/collections/<collection_id>/seg-<seg_id:06>/` 配下の 4〜5 ファイル。
seg_id は DB 全体で単調増加 (= 大きいほど新しい。newest-wins の判定に使う)。
セグメントは不変。フラッシュ時に一時ディレクトリ `seg-<id>.tmp` に全ファイルを
書いて fsync 後、rename して manifest に登録する。

### vectors.bin

```
header (64 B に pad):
  magic[8], dim u32, count u64, reserved
data:
  count × dim × f32  (行指向。data 先頭は 64B アラインメント)
footer:
  crc32c u32 (data 全体)
```

行番号 `row` (0..count) がセグメント内の物理位置。mmap して
`&[f32]` として直接距離計算に使う。

### ids.bin

```
header: magic[8], count u64
rows:   count × u64        # row 順の id
index:  count × (id u64, row u32)  # id 昇順。点参照用の二分探索テーブル
footer: crc32c u32
```

### meta.bin

```
header:  magic[8], count u64
offsets: (count+1) × u64   # blob 領域内のオフセット。row i の blob は [off[i], off[i+1])
blobs:   Metadata バイナリ表現の連結
footer:  crc32c u32
```

メタデータが空の行は off[i] == off[i+1]。

### tombstones.bin

このセグメントのフラッシュ時点で memtable に溜まっていた削除マーカーの集合。
**自分より古いセグメント**の行を無効化する。

```
header: magic[8], count u64
ids:    count × u64  (昇順)
footer: crc32c u32
```

### hnsw.bin (M3 で追加)

index 層 (docs/design/index.md §3) が定義。M2 の時点では存在しないことを許す
(存在しなければ Flat 検索にフォールバック)。

---

## 4. manifest と CURRENT

### MANIFEST-<gen:010>

```
magic[8]
gen u64                    # この manifest の世代
next_collection_id u32
next_seg_id u64
wal_seq u64                # この世代に反映済みの WAL seq (これ以下は不要)
collection_count u32
per collection:
  collection_id u32, name string, dim u32, metric u8
  seg_count u32
  per segment: seg_id u64, record_count u64, tombstone_count u64
crc32c u32 (全体)
```

### CURRENT

有効な manifest のファイル名 1 行のみ。更新手順:

1. `MANIFEST-<gen+1>` を書いて fsync
2. `CURRENT.tmp` に新ファイル名を書いて fsync
3. `rename(CURRENT.tmp, CURRENT)` + 親ディレクトリ fsync

どの時点でクラッシュしても CURRENT は完全な世代を指す。
旧 MANIFEST と不要 WAL の削除は rename 成功後 (失敗しても無害、次回起動で掃除)。

---

## 5. 復旧手順 (Database::open)

1. `<db_dir>` が空なら初期化: gen=0 の manifest と CURRENT を作成
2. CURRENT → manifest を読み、CRC 検証
3. 各 collection の各セグメントを mmap で開く (CRC はここでは検証しない。
   `verify_on_open` オプションで全検証可)
4. `wal/` の manifest.wal_seq より新しい WAL を seq 順にリプレイ → memtable 再構築
5. manifest.wal_seq 以下の WAL ファイル、CURRENT が指さない MANIFEST、
   `.tmp` の残骸を削除

---

## 6. memtable とフラッシュ

### memtable

```rust
struct Memtable {
    upserts: HashMap<Id, StoredRecord>,  // 正規化済みベクトル + メタデータ
    deletes: HashSet<Id>,                // 削除マーカー (upserts と排他)
    bytes: usize,                        // ベクトル+メタの概算バイト数
}
```

- upsert: `deletes.remove(id)` してから `upserts.insert`
- delete: `upserts.remove(id)`。**id が古いセグメントに存在するかに関わらず**
  `deletes.insert` (存在確認しない。tombstone が空振りしても無害)

### フラッシュ条件

`bytes >= flush_threshold` (既定 64 MiB) または明示的な `flush()` 呼び出し。

### フラッシュ手順

1. アクティブ memtable を immutable にし、新 memtable + 新 WAL (seq+1) に切り替え
   (この間だけ書き込みロック。以後の書き込みは新 WAL へ)
2. immutable memtable からセグメントファイル一式を `.tmp` に書き出す
   (upserts → vectors/ids/meta、deletes → tombstones。M3 以降はここで HNSW 構築)
3. rename でセグメントを確定
4. 新 manifest (セグメント追加 + wal_seq 更新) を書き、CURRENT を切り替え
5. immutable memtable と旧 WAL を破棄

v0 は 1〜5 を書き込みスレッド上で同期実行してよい (バックグラウンド化は M4)。
クラッシュ時: 4 の前に落ちれば旧 WAL が残っているのでリプレイで復元される。
4 の後に落ちれば新セグメントが有効で旧 WAL は無視される。どちらも一貫。

---

## 7. 複数ソースの読み取り (newest-wins)

検索・点参照は「memtable → セグメント (seg_id 降順)」の優先順で id を解決する。

- 点参照 `get(id)`: memtable.deletes にあれば None、memtable.upserts にあれば
  それ、なければ新しいセグメントから順に ids.bin を二分探索。
  途中のセグメントの tombstones に id があれば None
- 検索: 各ソースから上位 k を収集後、id ごとに**最も新しいソースの結果だけ残す**。
  古いソースのヒットは、より新しいソースに同 id の upsert または tombstone が
  あれば捨てる。判定には memtable と各セグメントの id 索引/tombstone 集合を使う

この判定を `LiveView` (スナップショット) として抽象化する:

```rust
struct LiveView {
    memtable: Arc<MemtableSnapshot>,
    segments: Vec<Arc<Segment>>,  // seg_id 降順
}
impl LiveView {
    /// source_rank (0=memtable, 1=最新セグメント, ...) のヒットが有効か
    fn is_live(&self, id: Id, source_rank: usize) -> bool { ... }
}
```

検索開始時に `Arc` 群を clone して LiveView を作れば、検索中にフラッシュや
コンパクションが走ってもセグメントは解放されない (Arc が保持)。

---

## 8. crate 構成とテスト

- 新 crate `crates/hamane-storage`。公開面: `Wal{Writer,Reader}`, `SegmentWriter`,
  `Segment`, `Manifest`, `Store` (上記すべてを束ねる collection 単位の永続化装置)
- `hamane::Collection` は内部を `RwLock<HashMap>` から `Store` に差し替える。
  **公開 API は不変**
- テスト:
  - フレーム/フォーマットのラウンドトリップ (ユニット)
  - WAL 末尾切り詰めの全バイト位置での復旧 (todos/210)
  - proptest: ランダムな upsert/delete/flush/reopen 系列を HashMap 参照実装と比較
    (todos/211)
