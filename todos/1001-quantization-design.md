# 1001: 量子化・クラスタリング索引の設計文書

- Status: DONE (2026-08-26)
- Milestone: M10
- Depends: なし
- Design: docs/design/quantization.md (このタスクの成果物)

## ゴール

PQ・IVF・IVF-PQ の v0 設計を確定する。既存の SQ8 / HNSW とフォーマット互換を
保ったまま opt-in で追加できることを示す。

## 成果

docs/design/quantization.md を作成。中心となる判断:

- 量子化・索引ファイルは全て**任意ファイル** (SQ8 の vectors_sq8.bin と同じ扱い)。
  無ければ従来動作にフォールバック。元の vectors.bin は常に残し f32 再ランク・
  再構築に使う
- PQ は ADC の距離クロージャを既存 `search_hnsw_by` に渡すだけで統合でき、
  hamane-index の変更ゼロ
- HNSW と IVF はどちらも枝刈り機構なので**セグメント単位で排他** (`IndexKind`)
- IVF-PQ は残差 PQ (IVFADC 標準)。粗 k-means とコードブック学習は共通実装
- k-means / コードブック処理は hamane-core/pq.rs に置き storage 非依存にする
  (sq8.rs と対称)
- 許可する index × quantization は 5 通りのみ (それ以外は open 時エラー)
