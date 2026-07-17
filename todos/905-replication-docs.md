# 905: 昇格手順の検証とドキュメント

- Status: TODO
- Milestone: M9
- Depends: 904
- Design: docs/design/replication.md §5, §7

## やること

- [ ] 昇格テスト: replica を通常モードで開き直して書き込めること
- [ ] 仕様書 (docs/spec) にレプリケーション章を追加
- [ ] README / docker-compose に replica 構成例
- [ ] レプリカ台数と検索スループットの実測 (docs/benchmarks.md)

## 完了条件

- 仕様書だけを読んで primary + replica + 昇格が再現できる
