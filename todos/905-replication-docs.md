# 905: 昇格手順の検証とドキュメント

- Status: DONE (2026-07-18)
- Milestone: M9
- Depends: 904
- Design: docs/design/replication.md §5, §7

## やること

- [x] 昇格テスト: 904 の E2E (compaction_and_promotion) で検証済み
- [x] 仕様書 (docs/spec) にレプリケーション章を追加
- [x] README / docker-compose に replica 構成例
- [x] レプリカ台数と検索スループットの実測 — **見送り**。レプリカは
      独立プロセスなので単一マシンでは同一 CPU を奪い合い、意味のある
      スケール実測にならない (複数マシンが要る)。read スケールは
      アーキテクチャ上プロセス数に線形

## 完了条件 — 達成

- 仕様書だけを読んで primary + replica + 昇格が再現できる
