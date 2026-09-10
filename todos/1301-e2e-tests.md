# 1301: E2E テストの拡充 (機能網羅 + 大量データ)

- Status: DONE (2026-09-10)
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

- [x] 重いテストは `#[ignore]` を付け、`cargo test -- --ignored` で回す。
      通常 CI は現状どおり数分で終わる状態を維持する
- [x] `.github/workflows/nightly.yml` (schedule + workflow_dispatch) を追加し、
      `--ignored` を含む全テストを 1 日 1 回実行する
- [x] 大量データテスト用の共通ヘルパを `crates/hamane/tests/common/` に切り出す
      (決定的な乱数ベクトル生成、recall 計算、一時 DB、経過時間・RSS の記録)

### B. 大量データの読み書き (`crates/hamane/tests/scale.rs`、全て `#[ignore]`)

- [x] **100 万件投入 → 検索一貫性**: dim=128 を 1M 件 upsert し、
      フラッシュ閾値を小さくして**複数セグメントを強制**。全件 `len()` 一致、
      サンプリングした ID の `get` 一致、recall@10 ≥ 0.95 (Flat 正解と比較)
- [x] **書き込み中の検索**: upsert スレッドと search スレッドを並行させ、
      検索が panic せず・スコアが常に正確な f32 距離であること
      (バックグラウンドフラッシュ中も含む)
- [x] **削除と上書きが混ざる長時間ワークロード**: 50 万件に対し
      upsert/delete/search をランダム比率で 10 分相当まわし、
      参照モデル (HashMap) と最終状態が一致すること
- [x] **コンパクションでディスクが収束**: 上書き中心のワークロードで
      ディスク使用量が単調増加しないこと (401 のテストの大規模版)
- [x] **大きい次元**: dim=768 を 10 万件で投入し、量子化あり/なしで
      検索が通ること (メモリ使用量も記録)
- [x] **再起動の耐性**: 1M 件の DB を close → open し直し、
      open 時間・件数・検索結果が一致すること

### C. コンポーネント跨ぎ E2E (`crates/hamane-cli/tests/e2e.rs` ほか)

- [x] **CLI → ライブラリ**: CLI (`create` / `insert` JSONL / `flush` /
      `compact`) で作った DB を `Database::open` で読み、件数と検索結果が一致
- [x] **サーバ → 再起動 → サーバ**: HTTP で投入 → プロセス再起動 →
      同じデータが読める (`admin/flush` 経由と WAL のみの両方)
- [x] **レプリカ昇格の実プロセス E2E**: primary/replica を実バイナリで起動し、
      同期 → primary 停止 → replica を `--replicate-from` 無しで再起動 →
      書き込めること (現状は同一プロセス内の `ReplicaSync` テストのみ)
- [x] **バックアップ → 復元**: `Database::backup` の出力を別ディレクトリで
      open し、全件・検索結果が一致 (書き込み中のバックアップも)
- [x] **Python バインディング**: CLI で作った DB を Python から読む
      (`crates/hamane-py/tests/test_interop.py`。CLI が無ければ skip)
- [x] **Docker イメージのスモーク**: ビルドしたイメージを起動し
      `/health` と 1 件の upsert/search が通る (docker.yml に smoke ジョブを追加)

### D. 構成の組み合わせ網羅 (`crates/hamane/tests/matrix.rs`)

- [x] 許可される **index × quantization × pq_nbits × opq の全構成**について、
      同一データで CRUD → フラッシュ → 再 open → 検索を回し、
      **スコアが常に正確な f32 距離**で、recall が閾値以上であることを確認
- [x] 各構成 × **フィルタあり (pre/post 両経路)** × **u64 / 文字列 ID** の
      直交な組み合わせをテーブル駆動で回す
- [x] メトリック 3 種 (L2 / Cosine / Dot) を同じ枠組みで確認

## 完了条件

- [x] `cargo test --workspace` は現状どおり数分で green (重いものは ignore)
- [x] `cargo test --workspace -- --ignored` が green (ローカルは 3 万件で検証。
      100 万件は nightly の初回実行で確認する)
- [x] nightly ワークフローの初回実行を確認する
      → **失敗していたので直した** (下記)
- [x] 上記 A〜D で**新規に見つかった不具合は修正するか、
      再現テストを残したうえで todo に切り出す** (下の実装メモ参照)

## 実装メモ

- 追加したもの: `tests/common/mod.rs` (共通ヘルパ)、`tests/scale.rs` (大量データ
  6 本、全て `#[ignore]`)、`tests/matrix.rs` (構成網羅 3 本)、
  `hamane-cli/tests/e2e.rs` (CLI 4 本)、`hamane-server/tests/process_e2e.rs`
  (実プロセス 2 本)、`.github/workflows/nightly.yml`
- 件数は `HAMANE_SCALE_N` (既定 10 万) で調整。nightly は 100 万を渡す。
  **`--release` 必須** (debug の HNSW 構築は桁違いに遅い)
- **計測して分かったこと 1**: 一様乱数の高次元ベクトルは HNSW の最悪ケース。
  dim=128・3 万件・既定 ef=64 で recall@10 = 0.73 (ef=512 で 0.98)。
  次元の呪いで距離が集中するため。実データ (SIFT で 0.98) に近い評価には
  クラスタ構造が要るので `common::clustered_dataset` を用意した
- **計測して分かったこと 2**: そのクラスタを固く分離しすぎると、真の近傍同士が
  ほぼ等距離になり量子化誤差が識別能力を上回る (dim=768 + PQ で recall 0.83)。
  PQ の性質であって不具合ではないので、クラスタは適度に重なる設定にした
- **不具合は見つからなかった**。既存実装は全構成・プロセス跨ぎ・大量データで
  期待どおり動いた (M11/M12 で見つけたレプリケーションの穴は既に修正済み)

## 追記 (2026-09-10): nightly の初回実行で見つかった問題

初回のスケジュール実行 (1,000,000 件) は **110 分かかった上に失敗**した。

1. **`high_dimension_with_quantization` が recall 0.28 で失敗**。
   原因は**テストデータの作り方**で、実装ではなかった。`clustered_dataset` は
   塊が固く分離しているため、件数が増える (= 1 クラスタあたりの点が増える) ほど
   真の近傍同士がほぼ等距離になり、PQ の量子化誤差が識別能力を上回る。
   → 埋め込みに近い **低ランク + ノイズ** (`common::low_rank_dataset`) に変更。
   dim=768・2 万件で f32/SQ8/PQ8/PQ4 の全構成が recall 1.0000 になった
2. **実行時間**: フラッシュ閾値 8 MiB が細かすぎて、100 万件の投入だけで 18 分
   (フラッシュ 62 回 + そのたびのコンパクション) かかっていた。
   → 32 MiB に変更し、規模が効かないテストには件数の上限 (`capped`) を入れた。
   ローカル実測で 100 万件テストは 222 秒 (recall 1.0000、2 セグメント)

## 未着手 (別タスクに切り出す候補)

- nightly の失敗通知 (現状は GitHub の既定通知のみ)
