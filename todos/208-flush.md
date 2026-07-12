# 208: フラッシュパイプラインと WAL ローテーション

- Status: DONE (2026-07-12)
- Milestone: M2
- Depends: 207
- Design: docs/design/storage.md §6

## ゴール

memtable が閾値を超えたらセグメントへ吐き出し、WAL を世代交代させる。
クラッシュがどのステップで起きても一貫性が保たれること。

## やること

- [ ] `CollectionConfig.flush_threshold_bytes` (既定 64 MiB) 追加
- [ ] フラッシュ手順 (storage.md §6 の 1〜5) を `Store::flush()` に実装
  - v0 は書き込みスレッド上で同期実行
- [ ] `Collection::flush()` を公開 API に追加
- [ ] `Collection::upsert_batch(Vec<Record>)` を追加 (WAL sync 1 回に集約)
- [ ] 旧 WAL / 旧 manifest の削除は manifest 切り替え成功後

## 完了条件

- 閾値を小さく設定した統合テスト: 大量 upsert → 自動フラッシュ →
  セグメントが生えて memtable が空になり、検索結果は不変
- flush 直後に再 open して WAL リプレイなしで全件見える
- flush → さらに書き込み → 再 open で「セグメント + WAL リプレイ」の合成が正しい
