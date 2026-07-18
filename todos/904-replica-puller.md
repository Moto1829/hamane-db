# 904: replica puller と --replicate-from

- Status: DONE (2026-07-18)
- Milestone: M9
- Depends: 902, 903
- Design: docs/design/replication.md §3, §5, §6

## ゴール

hamane-server を `--replicate-from <url>` で replica として起動できるようにする。

## やること

- [x] std::net の最小 HTTP GET クライアント (Content-Length 前提、todo 802 の
      --healthcheck と同系統)
- [x] 同期ループ: state → (世代差分ならスナップショット同期) → WAL tail 追記 +
      フレーム適用。404 は再同期で回復
- [x] fetch した WAL / セグメントは primary と同一レイアウトでディスクに保存
- [x] 書き込み系エンドポイントは 409 {"error": "read-only replica"}
- [x] /health に role / manifest_gen を追加 (lag_bytes は puller 内部
      状態の共有が必要になるため見送り。将来の監視強化で)
- [x] 結合テスト: WAL tail のみ / フラッシュ跨ぎ / コンパクション跨ぎ /
      フレーム途中切断の継続

## 実装メモ

- E2E テスト 3 本 (tail のみ / フラッシュ・コンパクション跨ぎ + 昇格 /
  再起動後の継続) + 実プロセス 2 台でのスモークテスト済み
- ローカル WAL には magic + 完全なフレームだけを書く (フレーム未完の
  fetch 分はメモリ持ち越し)。クラッシュしても通常リプレイで復元される
- hnsw.bin / vectors_sq8.bin は optional (404 はスキップ)。コンパクション
  競合の 404 は次のポーリングの再同期で収束

## 完了条件 — 達成

- upsert → 1 ポーリング周期以内に replica の検索へ反映
- 同期中のコンパクションから収束する
