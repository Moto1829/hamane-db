# レプリケーション

hamane-server は**単方向・非同期・pull 型**の read レプリカをサポートします
(設計の詳細は `docs/design/replication.md`)。

- replica は primary の HTTP API をポーリングして追従します。primary 側に
  レプリカの管理・設定は不要です (何台いても、いなくても同じ動作)
- replica は**読み取り専用**: 検索・点参照はそのまま使え、書き込み系
  エンドポイントは `409 {"error": "read-only replica: ..."}` を返します
- 一貫性は **prefix 一貫性**: replica の状態は常に「primary がある時点で
  持っていた状態」です。鮮度はポーリング間隔 (既定 1 秒) 程度遅れます

## 起動

```sh
# primary (通常の起動そのまま)
hamane-server --db ./primary-db --listen 0.0.0.0:8080 --api-key secret

# replica (別マシン / 別ディレクトリ)
hamane-server --db ./replica-db --listen 0.0.0.0:8081 --api-key secret \
    --replicate-from http://primary:8080
```

| フラグ | 環境変数 | 既定 | 説明 |
|---|---|---|---|
| `--replicate-from <url>` | `HAMANE_REPLICATE_FROM` | — | primary の URL。指定するとレプリカとして起動 |
| `--poll-interval-ms <n>` | `HAMANE_POLL_INTERVAL_MS` | 1000 | ポーリング間隔 |

API キーは primary と同じものを指定します (同期リクエストの認証にも
使われます)。

## 監視

`GET /health` (認証不要) が role と世代を返します:

```json
{"status": "ok", "role": "replica", "manifest_gen": 42}
```

primary と replica の `manifest_gen` を比較すれば世代の追従を確認できます
(フラッシュ間の WAL 追従はこれより細かく、ポーリングごとに進みます)。

## 昇格 (フェイルオーバー)

replica のディスクレイアウトは primary と同一に保たれるため、昇格は
**`--replicate-from` を外して起動し直すだけ**です:

```sh
hamane-server --db ./replica-db --listen 0.0.0.0:8080 --api-key secret
```

- 昇格後は通常の primary として書き込めます
- 自動フェイルオーバー・リーダー選出はありません (手動運用)
- 旧 primary が残っている場合の二重書き込み防止は運用側の責務です

## 仕組み (概要)

1. replica は `/replication/state` で primary の manifest 世代と
   アクティブ WAL の位置を確認する
2. 世代が進んでいれば、不足しているセグメントファイルと manifest を
   fetch して自分のディスクに同じレイアウトで配置し、世代を切り替える
   (フラッシュ・コンパクション後に起きる)
3. フラッシュ間の書き込みはアクティブ WAL の tail を fetch し、
   ローカル WAL への追記とインメモリ適用で追従する

manifest + 不変セグメントは常に完全なスナップショットなので、WAL を
取りこぼしても次の世代同期で必ず追いつきます (失敗モードは常に
「少し古い」に収斂し、一貫性は壊れません)。

## 制限

- 同期レプリケーション (書き込み ACK) はありません。primary が
  ディスクごと失われた場合、直近のポーリング間隔ぶんの書き込みは
  replica に届いていない可能性があります
- 同期は HTTP/1.1 の `Content-Length` 前提の直結を想定しています。
  chunked encoding になる中間プロキシ越しでは動きません
  (プロキシで buffering を切るか直結してください)
- collection 単位の部分レプリケーションはありません
