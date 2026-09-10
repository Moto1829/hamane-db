# 1303: nightly の実行時間短縮 (カバレッジは減らさない)

- Status: TODO
- Milestone: M13
- Depends: 1301
- Design: —

## ゴール

nightly (大量データ E2E) の実時間を **36 分 → 20 分以下**にする。
検証内容は減らさない。分数はパブリックリポジトリなので無料であり、
削るべきは「テストの本質に寄与しない準備時間」だけ。

## 現状の実測 (2026-09-10、`HAMANE_SCALE_N=1000000`、run 34490786679)

scale ジョブ 2088 秒 / full ジョブ 90 秒 (並列なので実時間 36 分)。

| テスト | 時間 | 割合 | 内訳 |
|---|---|---|---|
| `large_dataset_search_stays_consistent` | ~650s | 31% | 投入 184s + **フラッシュ 455s** (100 万件の HNSW 構築) |
| `mixed_workload_matches_reference_model` | 511s | 24% | 40 万操作 |
| `large_database_reopens_identically` | 378s | 18% | **投入 378s に対し検証は 19 ミリ秒** |
| `concurrent_writes_and_searches` | 271s | 13% | 19.8 万件の書き込み中に 2,441 回検索 |
| `high_dimension_with_quantization` | ~140s | 7% | dim=768 × 4 構成 |
| `compaction_keeps_disk_bounded` | 97s | 5% | 2 万件 × 6 ラウンド |

読み取り:

- `large_dataset` のフラッシュ 455 秒は**価値のある時間**。100 万件の HNSW
  構築こそ壊れやすいので、ここは短縮対象にしない
- `large_database_reopens` は 6 分かけて 50 万件を作り、検証は 19 ミリ秒。
  再 open の正しさに 50 万件は要らない
- `mixed_workload` の値打ちは操作の**組み合わせの多様性**であって回数ではない

## やること

- [ ] scale ジョブを **matrix で 3 分割**して並列実行する
      (分数は無料なので、並列化はほぼタダで実時間が縮む):
  - A: `large_dataset_search_stays_consistent` (~11 分)
  - B: `mixed_workload_matches_reference_model` + `large_database_reopens_identically`
  - C: `concurrent_writes_and_searches` + `high_dimension_with_quantization`
       + `compaction_keeps_disk_bounded`
- [ ] 分割は `cargo test ... -- --ignored <フィルタ>` で行う。
      各ジョブが何を走らせるかがワークフローを見て分かる形にする
- [ ] `large_database_reopens_identically` の件数上限を 50 万 → 15 万に下げる
      (`capped(2, 150_000)`)。検証内容は変わらない
- [ ] `mixed_workload_matches_reference_model` の操作数を見直す
      (現状 `ops = n * 2` で 40 万。多様性が保てる範囲で減らす)
- [ ] 分割後もキャッシュ (`Swatinem/rust-cache`) が効いてビルドが重複しないこと
      を確認する。効かないなら 1 度ビルドして成果物を共有する形を検討する

## 完了条件

- [ ] nightly の実時間が 20 分以下
- [ ] 実行されるテストの集合が現状と同一 (どのジョブも取りこぼしがない)
- [ ] 100 万件の `large_dataset_search_stays_consistent` は件数を落とさない
- [ ] nightly が green で、失敗時にどのジョブが落ちたか一目で分かる

## メモ

- 「全部入り」の再現は `cargo test --release --workspace -- --ignored` のまま
  ローカルで可能にしておく (分割はワークフロー側の都合)
- 現状の 36 分でも運用上は困らない (夜間実行・無料・タイムアウト 120 分)。
  これは効率の改善であって、緊急性のあるタスクではない
