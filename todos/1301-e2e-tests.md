# 1301: E2E テストの拡充 (機能網羅 + 大量データ)

- Status: TODO
- Milestone: M13
- Depends: なし (M12 までの全機能が対象)
- Design: —

## ゴール

「単体では通るが、組み合わせ・スケール・プロセス跨ぎで壊れる」類のバグを
機械的に捕まえる。現状の統合テストは**単一プロセス・数千件**が中心で、
次の 3 つが手薄:

1. **コンポーネント跨ぎ** (CLI / HTTP サーバ / Python が同じ DB を扱う経路)
2. **大量データ** (複数セグメント + コンパクション + 実メモリ・実ディスク)
3. **構成の組み合わせ網羅** (index × quantization × フィルタ × ID 種別)

現状: 統合テスト 45 本 (hamane 34 / server 13 / storage 5 / index 4)。
最大データ量は `hnsw_integration` の 3,000 件、`store` の自動フラッシュ系で
数万件。100 万件規模と実プロセス跨ぎは**ベンチ (hamane-bench) でしか触れていない**。

## やること

### A. 実行の枠組み

- [ ] 重いテストは `#[ignore]` を付け、`cargo test -- --ignored` で回す。
      通常 CI は現状どおり数分で終わる状態を維持する
- [ ] `.github/workflows/nightly.yml` (schedule + workflow_dispatch) を追加し、
      `--ignored` を含む全テストを 1 日 1 回実行する
- [ ] 大量データテスト用の共通ヘルパを `crates/hamane/tests/common/` に切り出す
      (決定的な乱数ベクトル生成、recall 計算、一時 DB、経過時間・RSS の記録)

### B. 大量データの読み書き (`crates/hamane/tests/scale.rs`、全て `#[ignore]`)

- [ ] **100 万件投入 → 検索一貫性**: dim=128 を 1M 件 upsert し、
      フラッシュ閾値を小さくして**複数セグメントを強制**。全件 `len()` 一致、
      サンプリングした ID の `get` 一致、recall@10 ≥ 0.95 (Flat 正解と比較)
- [ ] **書き込み中の検索**: upsert スレッドと search スレッドを並行させ、
      検索が panic せず・スコアが常に正確な f32 距離であること
      (バックグラウンドフラッシュ中も含む)
- [ ] **削除と上書きが混ざる長時間ワークロード**: 50 万件に対し
      upsert/delete/search をランダム比率で 10 分相当まわし、
      参照モデル (HashMap) と最終状態が一致すること
- [ ] **コンパクションでディスクが収束**: 上書き中心のワークロードで
      ディスク使用量が単調増加しないこと (401 のテストの大規模版)
- [ ] **大きい次元**: dim=768 を 10 万件で投入し、量子化あり/なしで
      検索が通ること (メモリ使用量も記録)
- [ ] **再起動の耐性**: 1M 件の DB を close → open し直し、
      open 時間・件数・検索結果が一致すること

### C. コンポーネント跨ぎ E2E (`crates/hamane-cli/tests/e2e.rs` ほか)

- [ ] **CLI → ライブラリ**: CLI (`create` / `insert` JSONL / `flush` /
      `compact`) で作った DB を `Database::open` で読み、件数と検索結果が一致
- [ ] **サーバ → 再起動 → サーバ**: HTTP で投入 → プロセス再起動 →
      同じデータが読める (`admin/flush` 経由と WAL のみの両方)
- [ ] **レプリカ昇格の実プロセス E2E**: primary/replica を実バイナリで起動し、
      同期 → primary 停止 → replica を `--replicate-from` 無しで再起動 →
      書き込めること (現状は同一プロセス内の `ReplicaSync` テストのみ)
- [ ] **バックアップ → 復元**: `Database::backup` の出力を別ディレクトリで
      open し、全件・検索結果が一致 (書き込み中のバックアップも)
- [ ] **Python バインディング**: CLI で作った DB を Python から読む
      (`crates/hamane-py/tests/` に追加)
- [ ] **Docker イメージのスモーク**: ビルドしたイメージを起動し
      `/health` と 1 件の upsert/search が通る (CI では docker.yml に相乗り)

### D. 構成の組み合わせ網羅 (`crates/hamane/tests/matrix.rs`)

- [ ] 許可される **index × quantization × pq_nbits × opq の全構成**について、
      同一データで CRUD → フラッシュ → 再 open → 検索を回し、
      **スコアが常に正確な f32 距離**で、recall が閾値以上であることを確認
- [ ] 各構成 × **フィルタあり (pre/post 両経路)** × **u64 / 文字列 ID** の
      直交な組み合わせをテーブル駆動で回す
- [ ] メトリック 3 種 (L2 / Cosine / Dot) を同じ枠組みで確認

## 完了条件

- [ ] `cargo test --workspace` は現状どおり数分で green (重いものは ignore)
- [ ] `cargo test --workspace -- --ignored` が 1M 件規模を含めて green
- [ ] nightly ワークフローが green で、失敗時に通知が出る
- [ ] 上記 A〜D で**新規に見つかった不具合は修正するか、
      再現テストを残したうえで todo に切り出す**
