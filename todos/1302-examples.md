# 1302: サンプルコードの拡充

- Status: TODO
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

- [ ] `quickstart.rs`: collection 作成 → upsert → 検索 → 削除。
      README と仕様書 getting-started の内容をそのまま実行可能な形に
- [ ] `metadata_filter.rs`: メタデータ付き投入と条件検索。
      pre-filter / post-filter が自動で切り替わることをログに出す
- [ ] `string_ids.rs`: UUID など文字列 ID の CRUD
- [ ] `bulk_load.rs`: 100 万件をバッチ投入する定石
      (`upsert_batch` / `SyncPolicy::Batch` / フラッシュ閾値 / 事前 `flush`)。
      進捗と所要時間、最終的なセグメント構成を表示する
- [ ] `quantization.rs`: **同じデータを 5 構成で作って比較**
      (f32 / SQ8 / PQ / OPQ / IVF-PQ、および 4bit)。
      recall・検索時間・ディスクサイズを表にして出す。
      「どれを選ぶか」の判断材料をコードで示す (docs/benchmarks.md の縮小版)
- [ ] `backup_restore.rs`: `backup` → 別ディレクトリで open → 一致確認
- [ ] `concurrent.rs`: 複数スレッドからの読み書き (単一ライタ・複数リーダの
      使い分けと、`Arc<Database>` の共有方法)

### B. サーバ / CLI / Python

- [ ] `crates/hamane-server/examples/` または `docs/spec/src/` に
      **HTTP API の呼び出し例**を curl と Rust (reqwest) の両方で
- [ ] `examples/replication/`: primary + replica を起動する
      docker-compose と手順 (既存の docker-compose.yml を出発点に)
- [ ] `crates/hamane-py/examples/`: numpy 配列からの投入・検索、
      pandas から流し込む例
- [ ] CLI の実用レシピを `docs/spec/src/cli.md` に追記
      (JSONL 生成 → insert → flush → search のワンライナー)

### C. 実用シナリオ

- [ ] `rag_pipeline.rs`: 文書チャンク → (疑似) 埋め込み → 投入 →
      クエリ埋め込み → 近傍検索 → 出典表示、までの一連。
      埋め込みモデルは外部依存を避けてハッシュベースの疑似実装にし、
      「実際は OpenAI/ローカルモデルの出力を入れる」とコメントで示す
- [ ] `recommendation.rs`: ユーザー × アイテムの内積検索 (Metric::Dot) と
      メタデータでの絞り込み

### D. 導線と保証

- [ ] `examples/README.md` (または `crates/hamane/examples/README.md`) に
      一覧と「何を学べるか」を書き、リポジトリ README からリンクする
- [ ] CI に `cargo build --workspace --examples` を追加し、
      サンプルが腐らないようにする
- [ ] 重いサンプル (bulk_load / quantization) は件数を引数で減らせるようにし、
      既定は数十秒で終わる規模にする

## 完了条件

- [ ] `cargo build --workspace --examples` が CI で green
- [ ] 各サンプルが引数なしで実行でき、数十秒以内に終わる
- [ ] README から examples 一覧に辿れて、量子化構成の選び方が
      **動くコードで**示されている
