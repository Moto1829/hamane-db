# サンプル集

すべて `cargo run --example <名前>` で動きます (重いものは `--release` 推奨)。
一時ディレクトリに DB を作り、終了時に消します。

| サンプル | 実行 | 学べること |
|---|---|---|
| [quickstart](quickstart.rs) | `cargo run --example quickstart` | collection 作成 → 投入 → 検索 → 更新 → 削除 → 再 open の一通り |
| [string_ids](string_ids.rs) | `cargo run --example string_ids` | UUID などの文字列 ID。`ext_id()` での取り出し、u64 との混在 |
| [metadata_filter](metadata_filter.rs) | `cargo run --release --example metadata_filter` | メタデータ条件つき検索。選択率による pre/post filter の自動切り替え |
| [quantization](quantization.rs) | `cargo run --release --example quantization [件数]` | **7 構成 (f32 / SQ8 / PQ 8bit / PQ 4bit / OPQ / IVF / IVF-PQ) を同じデータで比較**。recall・検索時間・構築時間・ディスクを表で出す |
| [bulk_load](bulk_load.rs) | `cargo run --release --example bulk_load [件数]` | 大量投入の定石。バッチ・SyncPolicy・フラッシュ閾値・compact |
| [concurrent](concurrent.rs) | `cargo run --release --example concurrent` | 単一ライタ・複数リーダ。`Arc` 共有と、フラッシュ中でも壊れない検索 |
| [backup_restore](backup_restore.rs) | `cargo run --example backup_restore` | 一貫性のあるバックアップと、コピーを open するだけの復元 |
| [rag_pipeline](rag_pipeline.rs) | `cargo run --release --example rag_pipeline` | RAG の検索側。チャンク分割 → 埋め込み → 検索 → 出典つきプロンプト組み立て |
| [recommendation](recommendation.rs) | `cargo run --release --example recommendation` | 内積 (`Metric::Dot`) での推薦、条件による絞り込み、item-to-item |
| [write_latency](write_latency.rs) | `cargo run --release --example write_latency` | バックグラウンドフラッシュ中の書き込みレイテンシ分布 (性能検証用) |

## 他のインターフェース

| 場所 | 内容 |
|---|---|
| [hamane-server/examples/http_client.rs](../../hamane-server/examples/http_client.rs) | HTTP API を Rust (reqwest) から呼ぶ。`cargo run -p hamane-server --example http_client -- <base-url> [api-key]` |
| [docs/spec の HTTP API リファレンス](../../../docs/spec/src/http.md) | 全エンドポイントの curl 例 |
| [hamane-py/examples/numpy_pandas.py](../../hamane-py/examples/numpy_pandas.py) | numpy 行列の一括投入、pandas DataFrame からの流し込みと結果の DataFrame 化 |
| [examples/replication/](../../../examples/replication/) | primary + read レプリカを docker compose で立てて昇格まで試す |
| [docs/spec の CLI リファレンス](../../../docs/spec/src/cli.md) | CSV/埋め込み出力からの投入、バックアップなどのレシピ |

## 補足

- **埋め込みは疑似実装**です (`rag_pipeline` / `recommendation`)。外部依存を
  増やさないためにハッシュや決定的な疑似乱数で作っています。実際には
  埋め込みモデルの出力を `Record::new(id, embedding)` に渡してください
- 構成の選び方は [docs/benchmarks.md](../../../docs/benchmarks.md) の実測と、
  `quantization` サンプルを自分のデータで走らせた結果で判断するのが確実です
- HTTP / CLI / Python から使う例は
  [仕様書](../../../docs/spec/src/getting-started.md) と
  [CLI リファレンス](../../../docs/spec/src/cli.md) にあります
