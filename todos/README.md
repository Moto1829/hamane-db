# 実装タスク索引

設計: [docs/DESIGN.md](../docs/DESIGN.md) と [docs/design/](../docs/design/)。
1 タスク ≒ 1 PR 相当の粒度。番号の百の位がマイルストーンに対応する。
着手したらタスクファイル冒頭の Status を更新する。

## 完了済み

- ✅ M0: workspace + hamane-core (型・距離計算・フィルタ)
- ✅ M1: インメモリ Flat 検索 + 公開 API (Database/Collection/SearchBuilder)
- ✅ M2: 永続化 (WAL + セグメント + manifest)
- ✅ M3: HNSW / M4: 運用品質 (2026-07-12 完了)
- ✅ M5: 性能とスケーラビリティ / M6: 機能拡張 (2026-07-14 完了)

## 横断

| # | タスク | Depends |
|---|---|---|
| ✅ [000](000-ci.md) | CI (GitHub Actions) | — |

## M2: 永続化 (WAL + セグメント + manifest)

| # | タスク | Depends |
|---|---|---|
| ✅ [201](201-storage-scaffold.md) | hamane-storage 雛形とフォーマット基盤 | — |
| ✅ [202](202-wal.md) | WAL writer/reader とリプレイ | 201 |
| ✅ [203](203-memtable.md) | Memtable の分離 (tombstone 対応) | 201 |
| ✅ [204](204-segment-writer.md) | セグメント書き出し | 201, 203 |
| ✅ [205](205-segment-reader.md) | セグメント読み込み (mmap) | 204 |
| ✅ [206](206-manifest.md) | manifest と CURRENT の原子的切り替え | 201 |
| ✅ [207](207-recovery.md) | Database::open と復旧 | 202, 205, 206 |
| ✅ [208](208-flush.md) | フラッシュパイプラインと WAL ローテーション | 207 |
| ✅ [209](209-read-path.md) | 複数ソース読み取り (LiveView, newest-wins) | 208 |
| ✅ [210](210-crash-tests.md) | クラッシュ耐性テスト | 209 |
| ✅ [211](211-property-tests.md) | プロパティテスト (参照実装比較) | 209 |

M2 完了条件: 210・211 が green (= DESIGN.md M2 の「クラッシュ耐性テスト green」) — **達成済み**。

実装メモ (設計からの意図的な差分):
- フラッシュは collection 単位でなく **DB 全体一括** (WAL 削除可否の判定を単純化)
- フラッシュ閾値・sync ポリシーは CollectionConfig でなく **StoreOptions**
  (`Database::open_with_options`) で指定。manifest に永続化しない実行時オプション
- 検索の live 判定は「収集後の除外」でなく **走査時の除外** (query.md §2 更新済み)。
  結果は常に正確な top-k

## M3: HNSW

| # | タスク | Depends |
|---|---|---|
| ✅ [301](301-hnsw-build.md) | HNSW 構築 (挿入・隣接選択) | — (hamane-index 内で独立) |
| ✅ [302](302-hnsw-search.md) | HNSW 探索 | 301 |
| ✅ [303](303-recall-tests.md) | 再現率テスト (recall@10 ≥ 0.95) | 302 |
| ✅ [304](304-hnsw-serialization.md) | hnsw.bin 直列化と mmap ロード | 302 |
| ✅ [305](305-hnsw-integration.md) | フラッシュ統合とマージ検索 | 303, 304, 209 |
| ✅ [306](306-filter-planner.md) | フィルタ戦略 (pre/post 自動選択) | 305 |
| ✅ [307](307-bench-harness.md) | SIFT1M ベンチハーネス | 305 |

M3 完了条件: SIFT1M で recall@10 ≥ 0.95 — **達成済み (既定 ef=64 で 0.977。
docs/benchmarks.md 参照)**。

実装メモ (設計からの意図的な差分):
- 論文の extendCandidates を構築時に常時有効化 (強クラスタデータの再現率対策)
- クエリ時の上位層降下は ef=1 の貪欲でなく ef=8 の探索
  (クラスタ誤選択からの復帰のため。recall_at_10_clustered で検証)
- HnswParams.seed は Option でなく u64 (既定 0)。フラッシュ時は seg_id で上書き
- hnsw パラメータと min_rows は CollectionConfig でなく StoreOptions で指定
- 303 のデータ規模は CI 時間の都合で n=4000 (設計は 10k。大規模は 307 で実測)

## M4: 運用品質

| # | タスク | Depends |
|---|---|---|
| ✅ [401](401-compaction.md) | コンパクション (size-tiered + tombstone GC) | 209 |
| ✅ [402](402-simd-bench.md) | SIMD 距離カーネルと criterion ベンチ | 307 |
| ✅ [403](403-cli.md) | hamane-cli | 305 |

M4 完了条件: 長時間書き込みでディスク使用量が収束 (401 のテストで検証) — **達成済み**。

実装メモ (設計からの意図的な差分):
- コンパクションは size-tiered でなく **full merge** (セグメント数 ≥ 閾値で全統合)。
  部分マージは manifest がセグメントの年代順リストを持てば可能 (将来最適化)
- 402 は 307 (SIFT1M ベースライン) を待たず先行実施。結果は docs/benchmarks.md
  (dim768 で l2 1.7x / dot 2.2x。目標 2x は dot のみ達成、l2 はメモリ帯域律速)

## M5: 性能とスケーラビリティ

| # | タスク | Depends |
|---|---|---|
| ✅ [501](501-parallel-hnsw-build.md) | HNSW 構築の並列化 (1M を 5 分以内に) | — |
| ✅ [502](502-build-params-tuning.md) | extendCandidates のパラメータ化と構築コスト削減 | 307 |
| ✅ [503](503-parallel-search.md) | 検索のソース並列化と live_len の O(1) 化 | — |
| ✅ [504](504-background-maintenance.md) | バックグラウンドフラッシュ・コンパクション | 501 |
| ✅ [505](505-group-commit.md) | WAL group commit (SyncPolicy::Batch) | — |
| ✅ [506](506-tiered-compaction.md) | size-tiered 部分コンパクション | 504 |
| ✅ [507](507-api-polish.md) | API 品質の小改善バックログ | — |

M5 完了条件 — **達成済み** (docs/benchmarks.md に実測記録):
- SIFT1M 構築 297.7 秒 (目標 300 秒以内、単一スレッド比 4.8x)
- フラッシュ中の upsert p99 = 8µs (目標 < 10ms)

実装メモ (計画からの意図的な差分):
- 502: SIFT では off で構築 20% 高速・recall 同等だが、クラスタデータで
  必須のため**既定は on を維持** (opt-out 可能)
- 506: tier 分けは「×4 区切りの階層」でなく universal compaction 風
  (最新側から同規模の連続 run をマージ)。明示 compact() は従来どおり full merge
- 505: SyncPolicy::Batch は max_delay 不要の leader-follower 方式
  (先着スレッドが fsync し後続が相乗り)
- 507 の「config 不一致検出」は構造上発生しない (collection() は保存済み
  config を返す) ため対象外

## M6: 機能拡張とエコシステム

| # | タスク | Depends |
|---|---|---|
| ✅ [601](601-string-ids.md) | 文字列 ID 対応 | — |
| ✅ [602](602-sq8-quantization.md) | スカラー量子化 (SQ8) + 再ランク | 307 |
| ✅ [603](603-http-server.md) | hamane-server (HTTP API) | 504 |
| ✅ [604](604-python-bindings.md) | Python バインディング (pyo3) | — |

実装メモ (計画からの意図的な差分):
- 601: 専用の extid.bin でなく **_ext_id メタデータ方式** (フォーマット変更
  ゼロ、open 時にセグメント走査 + WAL リプレイで辞書を再構築)
- 602: 次元別 min/max でなく**全次元共通スケール** (距離計算が純粋な整数演算に
  還元される)。u8 SIMD カーネルは未着手 (スカラーで自動ベクトル化任せ)
- 604: maturin 未導入環境のため CI はコンパイルチェックのみ。pytest は
  crates/hamane-py/tests/ (手順は同ファイル冒頭)

## M7: 運用ハードニング (2026-07-15 計画)

| # | タスク | Depends |
|---|---|---|
| ✅ [701](701-sq8-simd.md) | SQ8 の u8 SIMD カーネル | 602 |
| ✅ [702](702-process-lock.md) | プロセス排他ロック | — |
| ✅ [703](703-backup.md) | バックアップ API | 702 |
| ✅ [704](704-python-ci.md) | Python バインディングの CI (pytest + wheel) | 604 |

M7 完了 (2026-07-15)。実装メモ:
- 701: NEON のみ実装 (dim768 で f32 比 L2 4.2x / dot 3.8x、スカラーと完全一致)。
  AVX2 は検証環境がなく見送り (x86_64 はスカラー = 自動ベクトル化任せ)
- 702: flock (advisory)。クラッシュ時は OS が自動解放するため残骸なし。
  非 unix はベストエフォート
- 703: flush + state ロック保持でのコピー (コピー中は書き込み停止)。
  WAL は含まず manifest 世代として一貫
- 704: pytest 4 本をローカル (venv + maturin develop) と CI (ubuntu、
  wheel ビルド + pip install + pytest) の両方で実行

将来候補 (未タスク化): 検索スレッドプール化、レプリケーション (WAL シッピング)、
crates.io / PyPI 公開。
