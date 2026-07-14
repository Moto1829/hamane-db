# オンディスクフォーマット

フォーマットバージョン: **v2** (manifest のみ v2。v1 は読み込み互換)。
本章は互換実装・デバッグのための仕様です。
より詳細な設計背景はリポジトリの `docs/design/storage.md` /
`docs/design/index.md` を参照してください。

## 共通規約

- すべて**リトルエンディアン**
- 文字列は `len: u32` + UTF-8 バイト列
- 各ファイルは 8 バイトの magic で始まる: `b"HAMANE"` + 種別 1 文字 +
  バージョン 1 バイト (現在 `\x01`)
- チェックサムは **CRC32C** (Castagnoli)。セグメント・manifest は
  「ファイル先頭から footer 直前まで」を対象とした footer CRC を持つ

| ファイル | magic |
|---|---|
| WAL | `HAMANEW\x01` |
| vectors.bin | `HAMANEV\x01` |
| ids.bin | `HAMANEI\x01` |
| meta.bin | `HAMANEM\x01` |
| tombstones.bin | `HAMANET\x01` |
| manifest | `HAMANEF\x02` (v1 = `\x01` も読める) |
| hnsw.bin | `HAMANEH\x01` |
| vectors_sq8.bin | `HAMANEQ\x01` |

## WAL (`wal/<seq:020>.wal`)

magic の後にフレーム列が続く追記ログ:

```text
crc32c: u32 | len: u32 | body (len バイト)
body = type: u8 + payload
```

| type | レコード | payload |
|---|---|---|
| 1 | Upsert | collection_id u32, id u64, dim u32, f32×dim, metadata |
| 2 | Delete | collection_id u32, id u64 |
| 3 | CreateCollection | collection_id u32, name string, dim u32, metric u8 |
| 4 | DropCollection | collection_id u32 |

metric: 0 = L2, 1 = Cosine, 2 = Dot。
metadata: `count u32` + (key string, tag u8 + 値) の列。
tag: 0 = Str, 1 = Int(i64), 2 = Float(f64), 3 = Bool(u8)。

読み取りは先頭から順に行い、EOF・部分フレーム・CRC 不一致で停止する
(そこまでが有効)。

## セグメント (`collections/<cid>/seg-<id:06>/`)

不変。一度書かれたら変更されない。行順は **id 昇順**。

### vectors.bin

```text
[0..8)   magic
[8..12)  dim: u32
[12..20) count: u64
[20..64) パディング (データ部を 64B 境界に揃える)
[64..)   count × dim × f32 (行指向)
footer   crc32c: u32
```

### ids.bin

```text
magic + count: u64                    (16B)
rows:  count × u64                    行番号 → id
index: count × (id: u64, row: u32)    id 昇順の二分探索テーブル
footer crc32c
```

### meta.bin

```text
magic + count: u64
offsets: (count+1) × u64      行 i のメタデータ blob は [off[i], off[i+1])
blobs:   metadata 直列化の連結 (空メタデータは長さ 0)
footer   crc32c
```

### tombstones.bin

このセグメントより**古い**セグメントの行を無効化する削除 id 集合。

```text
magic + count: u64
ids:   count × u64 (昇順)
footer crc32c
```

### hnsw.bin (任意)

HNSW グラフの CSR 直列化。mmap でコピーなしに探索できる。

```text
header (64B): magic, node_count u32, max_level u8, pad×3,
              entry_point u32 (なければ u32::MAX), m u32, m0 u32
levels: node_count × u8 (4B 境界にパディング)
層ごと (level 0 → max_level):
  level_node_count u32
  node_ids:  level_node_count × u32 (昇順)
  offsets:   (level_node_count+1) × u32
  neighbors: offsets[last] × u32
footer crc32c
```

node は行番号 (u32) と一致する。

### vectors_sq8.bin (任意)

SQ8 量子化ベクトル (`StoreOptions.sq8` 有効時のみ)。

```text
header (64B): magic, dim u32, count u64, min f32, max f32, pad
data: count × dim × u8   (q = round((x − min) / s), s = (max−min)/255)
footer crc32c
```

min/max は全次元共通 (グローバルスケール)。

### 文字列 ID (`_ext_id` メタデータ)

文字列 ID は専用ファイルを持たない。内部 u64 (2^63 以降を採番) との対応は
各レコードの `_ext_id` メタデータとして meta.bin / WAL に載り、
open 時に走査して辞書を再構築する。

## manifest (`MANIFEST-<gen:010>` と `CURRENT`)

```text
magic
gen: u64
next_collection_id: u32
next_seg_id: u64
wal_seq: u64            # これ以下の WAL は反映済み (リプレイ不要)
collection_count: u32
collection ごと:
  collection_id u32, name string, dim u32, metric u8
  seg_count u32
  segment ごと: seg_id u64, record_count u64, tombstone_count u64
footer crc32c
```

セグメントリストは**年代順 (古い → 新しい)** (v2)。部分コンパクションの
結果が古い位置に入るため、seg_id 順とは限らない。v1 (seg_id 昇順 = 年代順)
はそのまま読める。

`CURRENT` は有効な manifest ファイル名 1 行。更新は
「新 manifest 書き込み + fsync → CURRENT.tmp + fsync → atomic rename →
ディレクトリ fsync」の手順で行われ、どの時点でクラッシュしても
完全な世代に復帰できる。

## 互換性ポリシー

- magic のバージョンバイトが実装と異なるファイルは `Corrupted` として拒否
- v0.1 時点では前方・後方互換の保証はない (v2 以降でマイグレーション方針を
  定義する予定)
