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
| ✅ [705](705-server-auth.md) | hamane-server の API キー認証 | 603 |

M7 完了 (2026-07-15)。実装メモ:
- 701: NEON (dim768 で f32 比 L2 4.2x / dot 3.8x、スカラーと完全一致)。
  **AVX2 (x86_64) は 2026-09-11 に追加** — maddubs は符号の都合で使えないので
  16 ビット展開 + madd_epi16。正しさは CI (ubuntu = x86_64) で検証、速度は未測定
- 702: flock (advisory)。クラッシュ時は OS が自動解放するため残骸なし。
  非 unix はベストエフォート
- 703: flush + state ロック保持でのコピー (コピー中は書き込み停止)。
  WAL は含まず manifest 世代として一貫
- 704: pytest 4 本をローカル (venv + maturin develop) と CI (ubuntu、
  wheel ビルド + pip install + pytest) の両方で実行
- 705: 静的 API キー (Bearer / X-Api-Key、定数時間比較)。キー未指定は
  認証なし + 起動時警告。TLS は引き続きリバースプロキシ前提

## M8: 検索基盤の改善 (2026-07-15 計画)

| # | タスク | Depends |
|---|---|---|
| ✅ [801](801-search-thread-pool.md) | 検索スレッドプール化 | 503 |
| ✅ [802](802-docker.md) | Docker イメージ | 603, 705 |

801 実装メモ: 検索ごとの thread::scope を Database 共有の常駐プール
(std のみ、遅延起動、worker = `StoreOptions::search_threads` − 1 本) に
置き換え。SIFT 200k / 2 セグメントで QPS +14% (docs/benchmarks.md 参照)。

## M9: レプリケーション (2026-07-18 計画)

設計: [docs/design/replication.md](../docs/design/replication.md)。
単方向・非同期・pull 型の read レプリカ。WAL 保持・ACK なし
(manifest + 不変セグメント = 完全スナップショットに常にフォールバック可能)。

| # | タスク | Depends |
|---|---|---|
| ✅ [901](901-replication-design.md) | 設計文書 | — |
| ✅ [902](902-replication-api.md) | primary 側 /replication API | 901 |
| ✅ [903](903-follower-mode.md) | hamane-storage の follower モード | 901 |
| ✅ [904](904-replica-puller.md) | replica puller と --replicate-from | 902, 903 |
| ✅ [905](905-replication-docs.md) | 昇格検証とドキュメント | 904 |

M9 完了 (2026-07-18)。実装メモ:
- WAL 保持・ACK なしの pull 型 (設計どおり)。primary はファイルを読むだけで
  エンジン変更ゼロ。replica はディスクレイアウトを primary と同一に保ち、
  昇格 = --replicate-from を外して開き直すだけ
- HTTP クライアントは std::net の最小実装 (Content-Length 前提、
  chunked 非対応 = 直結前提)
- 902〜904 で結合テスト 4 本 + E2E 3 本 + follower 単体 5 本

## M10: 量子化とクラスタリング索引 (2026-08-26 計画 / 2026-09-03 完了)

設計: [docs/design/quantization.md](../docs/design/quantization.md)。
PQ (直積量子化) → IVF (転置ファイル) → IVF-PQ (残差 PQ) を段階的に追加した。
既存の SQ8 / HNSW とフォーマット互換を保ち、全て opt-in (既定 off)。
量子化・索引ファイルは任意ファイルで、無ければ従来動作にフォールバック。

| # | タスク | Depends |
|---|---|---|
| ✅ [1001](1001-quantization-design.md) | 設計文書 | — |
| ✅ [1002](1002-pq-codebook.md) | PQ コードブック学習 (k-means, hamane-core/pq.rs) | 1001 |
| ✅ [1003](1003-pq-segment-search.md) | PQ セグメント統合と ADC 2 段階検索 | 1002 |
| ✅ [1004](1004-ivf-coarse.md) | IVF 粗量子化と nprobe 検索 | 1002 |
| ✅ [1005](1005-ivf-pq.md) | IVF-PQ (residual PQ) 統合 | 1003, 1004 |
| ✅ [1006](1006-quantization-bench.md) | 構成別ベンチとドキュメント | 1003, 1004, 1005 |

M10 完了 (2026-09-03)。SIFT 200k 実測 (docs/benchmarks.md): SQ8 は recall 同等で
QPS 約 2 倍、PQ は約 1.5 倍。IVF/IVF-PQ は全 recall 域で HNSW 系に劣る (200k では
グラフ枝刈りが優秀) が、億件規模のメモリ削減用途向け。全構成 recall@10 ≥ 0.95。

設計上の要点:
- PQ は ADC の距離クロージャを既存 `search_hnsw_by` に渡すだけで統合でき、
  hamane-index の変更ゼロ (SQ8 と同じ 2 段階検索・再ランク)
- HNSW と IVF はどちらも枝刈り機構なので**セグメント単位で排他** (`IndexKind`)
- k-means / コードブック処理は hamane-core/pq.rs に集約 (sq8.rs と対称)
- 許可する index × quantization は 5 通りのみ (それ以外は open 時エラー)

## M11: OPQ (回転付き PQ) (2026-09-06 完了)

設計: [docs/design/opq.md](../docs/design/opq.md)。
M10 で非ゴールにした OPQ を追加した。PQ のコードブック学習の**前段に直交回転を
学習して挟むだけ**で、探索経路・ファイル構成は M10 のまま。既定 off、opq.bin は
任意ファイルなので M10 で書いたセグメントもそのまま読める。

| # | タスク | Depends |
|---|---|---|
| ✅ [1101](1101-opq-design.md) | 設計文書 | M10 |
| ✅ [1102](1102-opq-rotation.md) | 回転行列の学習 (hamane-core/opq.rs) | 1101 |
| ✅ [1103](1103-opq-segment.md) | opq.bin のセグメント統合とクエリ回転 | 1102 |
| ✅ [1104](1104-opq-bench.md) | ベンチとドキュメント | 1103 |
| ✅ [1105](1105-opq-parametric-init.md) | パラメトリック初期値 (PCA + 固有値割り当て) | 1102 |

設計上の要点:
- 直交変換は L2 と内積を保存するので**検索の意味は変わらず**、量子化誤差だけ
  下がる。再ランクは生 f32 のままでスコアは不変
- R の更新は直交 Procrustes 問題の解 = 極因子。SVD を持ち込まず Newton 反復
  `Q ← ½(Q + Q⁻ᵀ)` と f64 Gauss-Jordan 逆行列だけで解く
- **初期値は乱数直交行列**。恒等から始めると軸に沿ったデータで極因子が恒等を
  返して 1 歩も動かない (恒等が局所最適)
- IVF-PQ では粗量子化も残差 PQ も回転後空間で行う (回転が L2 を保存するため)
- `StoreOptions.opq: bool` は `Quantization::Pq` 専用 (他は open 時エラー)
- 学習は「恒等初期値 / パラメトリック初期値 / 回転なし」を実測誤差で比較して
  選ぶため、
  **有効にして素の PQ より悪くなることはない**

M11 完了 (2026-09-06)。SIFT 200k 実測 (docs/benchmarks.md): 素の SIFT では
PQ と同等 (次元順の構造に位置分割が既に合っているため利得なし)、`--rotate-input`
で次元順の構造を壊すと PQ 0.9584 → OPQ 0.9699 と半分近くを取り戻す。QPS は PQ と
同等、構築は +35〜54%。**適用領域は「次元の並びに意味がないデータ」= 多くの
埋め込みモデル**。

## M12: 4-bit PQ サブコード (2026-09-06 完了)

設計: [docs/design/quantization.md](../docs/design/quantization.md) §8。
サブコードを 4 ビット (16 セントロイド) にし、2 個を 1 バイトに詰める。
ヘッダに nbits/ksub が最初から入っていたのでフォーマット変更はゼロ。

| # | タスク | Depends |
|---|---|---|
| ✅ [1201](1201-pq4-core.md) | hamane-core の nbits パラメータ化と 4bit 詰め | 1002 |
| ✅ [1202](1202-pq4-segment.md) | セグメント統合と StoreOptions.pq_nbits | 1201 |
| ✅ [1203](1203-pq4-bench.md) | ベンチとドキュメント | 1202 |
| ✅ [1204](1204-pq4-fused-lut.md) | バイト単位の合成 LUT (表引き半減) | 1202 |

SIFT 200k 実測 (docs/benchmarks.md): **同じコード長 (32 B/行) なら
4bit × m=64 が 8bit × m=32 より recall が高く (0.9863 vs 0.9838)、QPS も同等
(9492 vs 9402)、構築は 2.3 倍速い** (32.7 s vs 76.0 s)。m 据え置きの 4bit は
コード半分・QPS 12012 で recall 0.907 (ef を上げても改善しない = 律速は 1 段目)。

設計上の要点:
- 4bit は 1 段目が粗いので**再ランク候補を k×4 → k×8** に広げる
  (`rerank_factor(nbits)`)。これが無いと recall が大きく落ちる
- OPQ の回転学習にも nbits を渡す (実際に使うコード幅に対して最適化する)
- **バイト単位の合成 LUT** (1204) で表引き回数を半分に戻した。4bit は 1 バイトに
  2 サブコードが入るので、バイト値で直接引ける `(m/2)×256` の表を作れる

## M13: テストとサンプルの拡充 (2026-09-08 完了)

機能追加が M12 まで進み、**検証と導線**が追いついていない。
E2E テストは単一プロセス・数千件が中心で、サンプルは性能計測用の 1 本のみ。

| # | タスク | Depends |
|---|---|---|
| ✅ [1301](1301-e2e-tests.md) | E2E テストの拡充 (機能網羅 + 大量データ) | — |
| ✅ [1302](1302-examples.md) | サンプルコードの拡充 | — |
| ✅ [1303](1303-nightly-runtime.md) | nightly の実行時間短縮 (36 分 → 20 分以下) | 1301 |

要点:
- 重いテスト (1M 件など) は `#[ignore]` にして nightly ワークフローで回す。
  通常 CI は数分のまま維持する
- コンポーネント跨ぎ (CLI / HTTP / Python / Docker / レプリカ昇格) を
  実プロセスで確認する経路が無いので作る
- index × quantization × フィルタ × ID 種別の**組み合わせ網羅**をテーブル駆動で
- サンプルは `cargo build --workspace --examples` を CI に入れて腐らせない。
  特に M10〜M12 で増えた量子化構成の選び方を**動くコード**で示す

結果: 通常テスト 45 → 56 本 (+ scale 6 本は `#[ignore]`)、サンプル 1 → 10 本
(+ Python・HTTP・レプリカ構成)。
**一様乱数の高次元データは HNSW の最悪ケース**と分かったのが収穫
(dim=128・3 万件・ef=64 で recall 0.73。実データ相当のクラスタデータなら 0.99)。
**2026-09-10 追記**: nightly の初回実行が失敗していたので原因を追った。
高次元 PQ の recall 低下は**テストデータの作り方**の問題 (固く分離した
クラスタは PQ の最悪ケース) で、埋め込みに近い低ランク + ノイズに変えて解決。
実行時間も 110 分 → 数分規模に短縮した (フラッシュ閾値と件数上限)。
あわせて Python 相互運用 E2E・Docker スモーク・HTTP/Python サンプルを追加し、
その過程で **`nprobe` が HTTP と CLI に露出していない** 取りこぼし (M10) を発見・修正。
仕様書に **HTTP API リファレンス章** (`docs/spec/src/http.md`) を新設した。

## M15: 検索 API の拡張 (2026-09-11 完了)

機能は揃っていたが、**検索 API の使い勝手**に穴があった。

| # | タスク | Depends |
|---|---|---|
| ✅ [1501](1501-batch-search.md) | バッチ検索 (複数クエリを 1 回で) | 801 |
| ✅ [1502](1502-threshold-search.md) | スコア閾値つき検索 | — |

要点:
- バッチは **クエリ間**で並列化する (各クエリ内はセグメント逐次)。
  セグメントが 1 個の DB では従来の並列化が効かないので、ここが効く。
  実測 20 万件・単一セグメント・500 クエリで **8.7 倍** (docs/benchmarks.md)
- `LiveView` の取得をバッチで 1 回にまとめた
- 閾値は metric に応じた自然な向き。内部キーでは両方向とも
  `key <= key_from_score(t)` に畳めるが、**L2 だけは負の閾値が例外**
  (キーが距離の二乗なので符号が消える → 必ず 0 件が正しい)
- HTTP (`/search/batch`)・CLI (`--threshold`)・Python (`search_batch`) に露出

## M16: レコードの列挙と一括削除 (2026-09-13 完了)

運用で必要になる**データ操作の穴**を埋める。現状レコードを取り出す手段は
`get(id)` と近傍検索だけで全件列挙ができず、削除も ID 指定の 1 件ずつしかない。

| # | タスク | Depends |
|---|---|---|
| ✅ [1601](1601-scan-and-count.md) | レコードの列挙とカウント (scan / count) | 209 |
| ✅ [1602](1602-bulk-delete.md) | 条件による一括削除 | 1601 |

要点:
- `scan` は **id 昇順**で返し、`after` カーソルでページングできるようにする。
  各ソースが既に id 昇順なので k-way マージで `limit` に達したら打ち切る
- `delete_by_filter` は 1601 の走査で ID を集めて `delete_batch` に流す。
  WAL の sync を 1 回にまとめる (`upsert_batch` と対称)
- newest-wins と tombstone の解決は `LiveView::get` に任せ、マージ側は
  id の重複を 1 回にまとめるだけにした
- Rust / HTTP (`GET`・`DELETE /records`) / CLI (`scan` `count` `delete`) /
  Python (`scan` `count` `delete_by_filter` `delete_batch`) の全経路に露出

将来候補 (未タスク化): crates.io / PyPI 公開 (実装優先のため保留)、
PQ4 の SIMD fast-scan
(16 エントリ LUT のシャッフル評価。合成 LUT で 8bit 同等までは戻ったので、
さらに速くしたい場合のみ)。
