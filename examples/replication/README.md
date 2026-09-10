# レプリカ構成を動かす

primary 1 台 + read レプリカ 1 台を docker compose で立てる例です
(設計は [docs/design/replication.md](../../docs/design/replication.md)、
運用は [仕様書のレプリケーション章](../../docs/spec/src/replication.md))。

```sh
cd examples/replication
docker compose up -d --build
```

## 構成

| サービス | ポート | 役割 |
|---|---|---|
| primary | 8080 | 書き込み可。`/replication/*` でスナップショットと WAL を配る |
| replica | 8081 | 読み取り専用。1 秒ごとに primary をポーリングして追従 |

レプリカは `--replicate-from http://primary:8080` を付けて起動するだけです。
ディスクレイアウトは primary と同一なので、**昇格はこのオプションを外して
再起動するだけ**で済みます。

## 試す

```sh
KEY=secret
AUTH="Authorization: Bearer $KEY"
JSON="Content-Type: application/json"

# 1. primary に collection を作って投入する
curl -s -X PUT -H "$AUTH" -H "$JSON" -d '{"dim":4,"metric":"l2"}' \
  http://127.0.0.1:8080/collections/docs
curl -s -X POST -H "$AUTH" -H "$JSON" \
  -d '[{"id":1,"vector":[1,0,0,0]},{"id":2,"vector":[0,1,0,0]}]' \
  http://127.0.0.1:8080/collections/docs/records
curl -s -X POST -H "$AUTH" http://127.0.0.1:8080/admin/flush

# 2. しばらくするとレプリカ側でも読める (既定 1 秒間隔)
sleep 2
curl -s -H "$AUTH" http://127.0.0.1:8081/collections/docs
# {"name":"docs","dim":4,...,"len":2,...}

# 3. レプリカへの書き込みは 409 (ReadOnlyReplica)
curl -s -o /dev/null -w '%{http_code}\n' -X POST -H "$AUTH" -H "$JSON" \
  -d '{"id":3,"vector":[0,0,1,0]}' \
  http://127.0.0.1:8081/collections/docs/records
# 409

# 4. 追従状況は /health で見る (認証不要)
curl -s http://127.0.0.1:8080/health   # {"role":"primary","manifest_gen":1,...}
curl -s http://127.0.0.1:8081/health   # {"role":"replica","manifest_gen":1,...}
```

## 昇格 (フェイルオーバー)

primary が落ちたら、レプリカから `--replicate-from` を外して再起動します。

```sh
docker compose stop primary
docker compose run --rm --service-ports \
  -e HAMANE_REPLICATE_FROM= -p 8081:8080 replica
# → role が primary になり、書き込みを受け付ける
```

`docker compose down -v` で後片付けします (`-v` はデータボリュームも消します)。
