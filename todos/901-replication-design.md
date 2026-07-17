# 901: レプリケーション設計文書

- Status: DONE (2026-07-18)
- Milestone: M9
- Depends: なし
- Design: docs/design/replication.md (このタスクの成果物)

## ゴール

WAL シッピングによる単方向・pull 型 read レプリカの v0 設計を確定する。

## 成果

docs/design/replication.md を作成。中心となる判断:

- manifest + 不変セグメント = 完全なスナップショットなので
  **WAL 保持・ACK プロトコルが不要** (取りこぼしはセグメント同期で収束)
- primary はレプリケーションを意識しない (ファイルを読むだけの HTTP API)
- replica のディスクレイアウトを primary と同一に保ち、昇格 = 開き直すだけ
- prefix 一貫性 (単一ライタ + WAL 順序再生)
