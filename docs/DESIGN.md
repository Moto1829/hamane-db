# hamane-db 設計ドキュメント

Rust 製の組み込み型ベクトルデータベースエンジン。まずは「ベクトル版 SQLite」のような
単一プロセス組み込みライブラリとして完成させ、その上にサーバ層を後から載せられる構成を取る。

- Status: Draft v0.2 (2026-07-12)
- 対象読者: 本プロジェクトの実装者

本書は概要設計。サブシステムごとの詳細仕様は以下:

- [design/storage.md](design/storage.md) — WAL・セグメント・manifest のフォーマット、復旧手順、フラッシュ
- [design/index.md](design/index.md) — HNSW の構築・探索・永続化、フィルタ戦略
- [design/query.md](design/query.md) — 検索実行計画、スナップショット分離、公開 API 最終形

実装タスクは [../todos/](../todos/) に粒度分割済み (README.md が索引)。

---

## 1. ゴールと非ゴール

### ゴール

- 数百万件規模のベクトル(〜数千次元)に対する近似最近傍探索 (ANN) を低レイテンシで提供する
- 挿入・削除・検索がクラッシュ後も一貫性を保つ永続化 (WAL ベース)
- メタデータ(任意の key-value)によるフィルタ付き検索
- 依存の少ない pure Rust 実装。unsafe は mmap / SIMD 境界に限定する
- 明快な Rust API(組み込みライブラリとして `hamane` クレートを公開)

### 非ゴール(当面)

- 分散・レプリケーション・シャーディング
- マルチテナント認証やアクセス制御
- SQL 互換クエリ言語
- GPU 対応

---

## 2. 全体アーキテクチャ

```
                    ┌─────────────────────────────┐
                    │        Public API (hamane)   │
                    │  Database / Collection ハンドル │
                    └──────────────┬──────────────┘
                                   │
              ┌────────────────────┼────────────────────┐
              │                    │                     │
     ┌────────▼───────┐   ┌───────▼────────┐   ┌────────▼───────┐
     │  Query Engine   │   │  Write Path     │   │  Catalog        │
     │  (探索+フィルタ) │   │  (WAL→memtable) │   │  (スキーマ管理)  │
     └────────┬───────┘   └───────┬────────┘   └────────────────┘
              │                    │
     ┌────────▼────────────────────▼────────┐
     │             Index Layer               │
     │   Flat (正確検索) / HNSW (近似検索)     │
     └────────┬──────────────────────────────┘
              │
     ┌────────▼──────────────────────────────┐
     │            Storage Layer               │
     │  WAL + 不変セグメント (mmap) + manifest │
     └───────────────────────────────────────┘
```

### データモデル

- **Database**: ディレクトリ 1 つに対応する永続化単位
- **Collection**: 次元数・距離関数を固定したベクトルの集合(RDB のテーブル相当)
- **Record**: `id (u64 or string) + vector (f32 配列) + metadata (JSON 風の key-value)`

### 距離関数

L2(ユークリッド)、コサイン、内積の 3 種を Collection 作成時に固定する。
コサインは挿入時に正規化して内積に還元する。

---

## 3. ストレージ層

LSM 風の「WAL + 不変セグメント」構成を取る。ベクトルは更新よりも追記が支配的な
ワークロードなので、この構成が単純さと性能を両立する。

### 書き込みパス

1. 変更(upsert / delete)を WAL に追記し fsync(バッチ可)
2. インメモリの **memtable**(ID → レコード)に反映
3. memtable が閾値(件数 or バイト数)を超えたら **セグメント** としてフラッシュ

### セグメント

- 不変 (immutable)。一度書いたら追記も更新もしない
- 内容: ベクトル本体(行指向・アラインメント済み f32)、ID テーブル、メタデータ列、
  および構築済みインデックス(HNSW グラフ等)を同一ファイル群に格納
- 読み取りは mmap で行い、OS のページキャッシュに委ねる
- 削除は **tombstone**(削除 ID 集合)で表現し、コンパクション時に物理削除

### manifest

現在有効なセグメントの一覧・世代番号を manifest ファイルで管理する。
セグメントの追加・削除は「新 manifest を書いて atomic rename」で切り替え、
クラッシュ時はどちらかの完全な世代に復帰できることを保証する。

### コンパクション

バックグラウンドで小さいセグメント同士をマージし、tombstone を適用して
新セグメントを構築する。方針は当初サイズ階層型 (size-tiered) の単純なもので良い。

### ディレクトリレイアウト

```
<db_dir>/
  MANIFEST-000042
  CURRENT                  # 有効な manifest 名を指す
  wal/000123.wal
  collections/<name>/
    seg-000001/{vectors.bin, ids.bin, meta.bin, hnsw.bin}
    seg-000002/...
```

### オンディスクフォーマット共通規約

- リトルエンディアン固定、各ファイル先頭に magic + フォーマットバージョン
- 各ブロックに CRC32C チェックサム(WAL はレコード単位)

---

## 4. インデックス層

`VectorIndex` trait で抽象化し、実装を差し替え可能にする。

```rust
trait VectorIndex {
    fn search(&self, query: &[f32], k: usize, filter: Option<&Filter>) -> Vec<(RowId, f32)>;
    fn build(vectors: &VectorStore, params: &IndexParams) -> Self;
}
```

### Flat(ブルートフォース)

- 全件スキャンの正確検索。SIMD(`std::simd` または手書き intrinsics)で距離計算を高速化
- memtable と小セグメントの検索、および HNSW の再現率を測る基準として常に保持する

### HNSW(主インデックス)

- セグメントフラッシュ時に構築し、セグメントと共に永続化(mmap で zero-copy ロード)
- パラメータ: `M`(接続数、既定 16)、`ef_construction`(既定 200)、`ef_search`(クエリ時指定可)
- memtable 部分は Flat で検索し、各セグメントの HNSW 結果とマージする
  (= 書き込み直後のデータも即座に検索可能)

### フィルタ戦略

- フィルタの選択率が高い(絞り込みが緩い)場合: HNSW 探索中に候補をフィルタで棄却 (post-filter with oversampling)
- 選択率が低い(強く絞られる)場合: 先にメタデータで ID 集合を作り Flat で検索 (pre-filter)
- 閾値による自動切り替えをクエリプランナに持たせる

### 将来拡張(v0 では対象外)

- スカラー量子化 (SQ8) / 直積量子化 (PQ) によるメモリ削減
- IVF 系インデックス

---

## 5. クエリエンジンと並行性

- **単一ライタ・複数リーダ** (single-writer / multi-reader)
- 検索はスナップショット越しに実行: manifest 世代 + memtable のコピーオンライト参照を
  `Arc` で保持し、検索中にセグメントが消えないことを保証(epoch ベースの解放)
- 書き込みは内部の writer スレッド(またはロック)に直列化。API 自体は `&self` で
  スレッドセーフに呼べるようにする

クエリの流れ:

```
Query { vector, k, filter, ef_search }
  → プランナ(pre/post フィルタ選択)
  → memtable (Flat) + 各セグメント (HNSW) を並列検索
  → tombstone 除去 → k 件にマージ → メタデータ付与して返却
```

---

## 6. 公開 API(案)

```rust
let db = Database::open("path/to/db")?;

let col = db.create_collection("docs", CollectionConfig {
    dim: 768,
    metric: Metric::Cosine,
    ..Default::default()
})?;

col.upsert(Record::new(42u64, vec![0.1; 768]).with_meta("lang", "ja"))?;
col.delete(42u64)?;

let hits = col.search(&query_vec)
    .k(10)
    .filter(Filter::eq("lang", "ja"))
    .ef(64)
    .run()?;
```

エラーは `thiserror` ベースの `HamaneError` に集約する。

---

## 7. クレート構成

Cargo workspace で分割する:

| クレート | 役割 |
|---|---|
| `hamane` | 公開 API(Database / Collection)。他クレートを束ねるファサード |
| `hamane-core` | 型定義(Record, Filter, Metric, エラー)、距離計算 (SIMD) |
| `hamane-index` | `VectorIndex` trait、Flat、HNSW 実装 |
| `hamane-storage` | WAL、セグメント、manifest、コンパクション |
| `hamane-cli` | 動作確認・ベンチ用 CLI(後回し可) |

サーバ層(gRPC/HTTP)は将来 `hamane-server` として追加するが、v0 のスコープ外。

主要な外部依存は最小限に: `thiserror`, `serde`(メタデータ), `memmap2`, `crc32c`,
`rand`(HNSW レベル抽選)程度を想定。

---

## 8. テストと検証

- **ユニットテスト**: 距離計算の数値検証、WAL リプレイ、manifest 切り替え
- **再現率テスト**: HNSW の結果を Flat の正解と比較し recall@10 ≥ 0.95(既定パラメータ)を CI で担保
- **クラッシュ耐性テスト**: 書き込み途中で強制終了 → 再オープンで一貫性が保たれることを検証
  (WAL の任意バイトで切り詰めるテストハーネス)
- **プロパティテスト**: `proptest` で upsert/delete/search のランダム系列を Flat 参照実装と突き合わせ
- **ベンチマーク**: `criterion` + 公開データセット (SIFT1M 等) で QPS / recall / build time を計測

---

## 9. マイルストーン

| # | 内容 | 完了条件 |
|---|---|---|
| M0 | workspace 雛形 + `hamane-core`(型・距離計算) | 距離計算のユニットテスト green |
| M1 | インメモリ Flat 検索 + フィルタ | upsert/delete/search が API 経由で動く |
| M2 | 永続化(WAL + セグメント + manifest) | クラッシュ耐性テスト green |
| M3 | HNSW(構築・永続化・マージ検索) | SIFT1M で recall@10 ≥ 0.95 |
| M4 | コンパクション + ベンチ整備 + CLI | 長時間書き込みでディスクが収束する |

---

## 10. 主な設計判断の記録

| 判断 | 理由 |
|---|---|
| 組み込みライブラリ先行、サーバ後回し | コア(索引・永続化)の品質に集中でき、テストも容易 |
| LSM 風 (WAL + 不変セグメント) | 追記支配のワークロードに合い、不変性が mmap・並行読みと相性が良い |
| HNSW を主インデックスに採用 | ディスク常駐前提でも実績があり、再現率/速度のバランスが良い |
| セグメントごとに独立インデックス | インクリメンタル構築が不要になり、実装が大幅に単純化 |
| コサインを正規化+内積に還元 | 距離計算カーネルが 2 種 (L2, dot) で済む |
