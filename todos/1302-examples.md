# 1302: サンプルコードの拡充

- Status: DONE (2026-09-10)
- Milestone: M13
- Depends: なし
- Design: —

## ゴール

「読めば使い始められる」実行可能なサンプルを揃える。現状 `examples/` は
`write_latency.rs` 1 本だけで、しかも**性能計測用**。M10〜M12 で増えた
量子化・索引の選択肢 (SQ8 / PQ / OPQ / IVF / IVF-PQ / 4bit) も、
仕様書に説明はあるが**動くコードが無い**。

サンプルは `cargo run --example <name>` で必ず動くこと (CI でコンパイルを保証)。

## やること

### A. Rust ライブラリ (`crates/hamane/examples/`)

- [x] `quickstart.rs`: collection 作成 → upsert → 検索 → 削除。
      README と仕様書 getting-started の内容をそのまま実行可能な形に
- [x] `metadata_filter.rs`: メタデータ付き投入と条件検索。
      pre-filter / post-filter が自動で切り替わることをログに出す
- [x] `string_ids.rs`: UUID など文字列 ID の CRUD
- [x] `bulk_load.rs`: 100 万件をバッチ投入する定石
      (`upsert_batch` / `SyncPolicy::Batch` / フラッシュ閾値 / 事前 `flush`)。
      進捗と所要時間、最終的なセグメント構成を表示する
- [x] `quantization.rs`: **同じデータを 5 構成で作って比較**
      (f32 / SQ8 / PQ / OPQ / IVF-PQ、および 4bit)。
      recall・検索時間・ディスクサイズを表にして出す。
      「どれを選ぶか」の判断材料をコードで示す (docs/benchmarks.md の縮小版)
- [x] `backup_restore.rs`: `backup` → 別ディレクトリで open → 一致確認
- [x] `concurrent.rs`: 複数スレッドからの読み書き (単一ライタ・複数リーダの
      使い分けと、`Arc<Database>` の共有方法)

### B. サーバ / CLI / Python

- [x] `crates/hamane-server/examples/` または `docs/spec/src/` に
      **HTTP API の呼び出し例**を curl と Rust (reqwest) の両方で
      (`docs/spec/src/http.md` を新設 + `examples/http_client.rs`)
- [x] `examples/replication/`: primary + replica を起動する
      docker-compose と手順 (既存の docker-compose.yml を出発点に)
- [x] `crates/hamane-py/examples/`: numpy 配列からの投入・検索、
      pandas から流し込む例 (`numpy_pandas.py`)
- [x] CLI の実用レシピを `docs/spec/src/cli.md` に追記
      (CSV/埋め込み出力からの投入、バックアップ、jq での後処理)

### C. 実用シナリオ

- [x] `rag_pipeline.rs`: 文書チャンク → (疑似) 埋め込み → 投入 →
      クエリ埋め込み → 近傍検索 → 出典表示、までの一連。
      埋め込みモデルは外部依存を避けてハッシュベースの疑似実装にし、
      「実際は OpenAI/ローカルモデルの出力を入れる」とコメントで示す
- [x] `recommendation.rs`: ユーザー × アイテムの内積検索 (Metric::Dot) と
      メタデータでの絞り込み

### D. 導線と保証

- [x] `examples/README.md` (または `crates/hamane/examples/README.md`) に
      一覧と「何を学べるか」を書き、リポジトリ README からリンクする
- [x] CI に `cargo build --workspace --examples` を追加し、
      サンプルが腐らないようにする
- [x] 重いサンプル (bulk_load / quantization) は件数を引数で減らせるようにし、
      既定は数十秒で終わる規模にする

## 完了条件

- [x] `cargo build --workspace --examples` が CI で green
- [x] 各サンプルが引数なしで実行でき、数十秒以内に終わる (最長は quantization の約 30 秒)
- [x] README から examples 一覧に辿れて、量子化構成の選び方が
      **動くコードで**示されている

## 実装メモ

- 9 本を追加 (既存の write_latency と合わせて 10 本)。一覧は
  `crates/hamane/examples/README.md`、README からリンク済み
- 埋め込みは外部依存を避けてハッシュ / 疑似乱数で作った。
  rag_pipeline は「意味は捉えない」ことを出力にも明記している
- `quantization` サンプルは 7 構成を同じデータで作り直して比較する。
  10,000 件 × dim=64 の実測では f32/SQ8 が最速 (23µs)、PQ 系が 32〜35µs、
  IVF-PQ が 121µs。OPQ は**件数が少ないほど構築コストが割高**
  (この規模で PQ の 6 倍。SIFT 20 万件では 1.5 倍)
- 検索時間は最初の 1 回が mmap のページフォルトで 10 倍以上遅く出るので、
  計測前にウォームアップを入れてある (最初これで OPQ が異常に遅く見えた)

## 追記 (2026-09-10)

B (サーバ / CLI / Python) を完了。作業中に **仕様書に HTTP API の
リファレンス章が無い**ことに気づいたので `docs/spec/src/http.md` を新設した
(全エンドポイントの curl 例、認証、エラー、フィルタの JSON 表現)。

さらに **`nprobe` が HTTP にも CLI にも露出していない**ことが判明した
(M10 で `SearchBuilder::nprobe` を足したときの取りこぼし)。IVF 構成では
検索ごとの調整ができない状態だったので、両方に追加し回帰テストを置いた
(`search_accepts_ef_and_nprobe` / `cli_search_accepts_nprobe`)。
