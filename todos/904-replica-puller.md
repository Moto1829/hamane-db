# 904: replica puller と --replicate-from

- Status: TODO
- Milestone: M9
- Depends: 902, 903
- Design: docs/design/replication.md §3, §5, §6

## ゴール

hamane-server を `--replicate-from <url>` で replica として起動できるようにする。

## やること

- [ ] std::net の最小 HTTP GET クライアント (Content-Length 前提、todo 802 の
      --healthcheck と同系統)
- [ ] 同期ループ: state → (世代差分ならスナップショット同期) → WAL tail 追記 +
      フレーム適用。404 は再同期で回復
- [ ] fetch した WAL / セグメントは primary と同一レイアウトでディスクに保存
- [ ] 書き込み系エンドポイントは 409 {"error": "read-only replica"}
- [ ] /health に role / manifest_gen / lag を追加
- [ ] 結合テスト: WAL tail のみ / フラッシュ跨ぎ / コンパクション跨ぎ /
      フレーム途中切断の継続

## 完了条件

- upsert → 1 ポーリング周期以内に replica の検索へ反映
- 同期中のコンパクションから収束する
