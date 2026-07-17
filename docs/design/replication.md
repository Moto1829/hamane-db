# 詳細設計: レプリケーション (WAL シッピング)

- Status: v0 設計 (2026-07-18、todo 901)
- 対象: M9。単方向・非同期・pull 型の read レプリカ

## 0. スコープ

やること (v0):

- **primary → replica の単方向レプリケーション**。replica は読み取り専用
- **pull 型**: replica が primary の HTTP API をポーリングする。
  primary は replica を管理しない (何台いても、いなくても同じ動作)
- **prefix 一貫性**: replica の状態は常に「primary がある時点で持っていた状態」
  (単一ライタ + WAL の順序がそのまま再生されるため)
- **手動昇格**: replica のディスクレイアウトは primary と同一に保つ。
  昇格 = puller を止めて通常モードで開き直すだけ

やらないこと (将来):

- 同期レプリケーション (書き込み ACK)、自動フェイルオーバー、リーダー選出
- マルチプライマリ、コンフリクト解決
- 部分レプリケーション (collection 単位のフィルタ)

## 1. 中心となる設計判断: WAL 保持は不要

Litestream 型の WAL シッピングは「rotate された WAL を replica が取得するまで
保持する」仕組み (retention + ACK) が必要になる。hamane-db では**これを丸ごと
省略できる**:

- manifest (gen) + 不変セグメント一式は、それ自体が `wal_seq` 時点の
  **完全で一貫したスナップショット** (バックアップ 703 と同じ性質)
- したがって replica が WAL を取りこぼしても、**次の manifest 世代へ
  セグメント同期すれば必ず追いつける**。失われるのは鮮度だけで、
  一貫性は失われない
- WAL tail の転送は「フラッシュ間の鮮度を上げる差分」にすぎない

この結果、primary 側は**通常運用のまま何も保持しない** (rotate 済み WAL は
従来どおり即削除してよい)。レプリケーションの失敗モードはすべて
「replica が少し古い」に収斂する。

## 2. プロトコル (HTTP、hamane-server に同居)

すべて既存の API キー認証の内側。パスは `/replication/*`:

| メソッド | パス | 内容 |
|---|---|---|
| GET | /replication/state | `{ manifest_gen, manifest_name, wal_seq, wal_len }` (アクティブ WAL の seq と現在長) |
| GET | /replication/manifest/{gen} | MANIFEST ファイルのバイト列 (書き込み後は不変) |
| GET | /replication/segment/{collection_id}/{seg_id}/{file} | セグメントファイル (vectors.bin 等。不変。file はホワイトリスト検証) |
| GET | /replication/wal/{seq}?offset=N | WAL の offset 以降のバイト列 (append-only) |

- レスポンスはすべて `Content-Length` 付きの単純なボディ
  (replica 側クライアントを std::net の最小実装にするため。§6)
- ファイルは不変 or append-only なので、primary 側はロックなしで
  ファイルを読むだけでよい。**ストレージエンジンへの変更はゼロ**
- 競合は 2 つだけ、どちらも replica 側でリトライすれば解消する:
  - コンパクション後の旧セグメント削除 → 404 → replica は state から同期し直す
  - WAL rotate → 404 or state の wal_seq 前進 → 同上

## 3. replica の同期ループ

```
loop:
    s = GET /replication/state
    if s.manifest_gen > local.gen:
        # スナップショット同期 (初回 / フラッシュ・コンパクション後)
        fetch MANIFEST-{gen} → .tmp
        manifest を parse し、ローカルにないセグメントのファイルを fetch → .tmp
        rename でセグメント確定 → CURRENT を切り替え (ローカルの open と同じ原子性)
        Store を新世代に切り替え、follower memtable を捨てる
        ローカルの不要ファイル (旧セグメント・古い WAL) を削除
    # WAL tail 同期 (鮮度)
    if s.wal_seq == local.tailing_seq:
        bytes = GET /replication/wal/{seq}?offset={local.wal_offset}
        ローカルの wal/{seq}.wal に追記 (ディスクレイアウトを primary と同一に保つ)
        完全なフレームまでを follower memtable に適用し、offset を進める
        (末尾の不完全フレームは次回に持ち越し。CRC 不一致も同様に待つ)
    sleep(poll_interval)      # 既定 1s
```

- WAL フレームは既存のリプレイと同じ形式・同じ検証 (CRC + 長さ)。
  「末尾の不完全フレームで停止」というローカル復旧のルールが
  そのままネットワーク越しの部分転送に適用できる
- 適用は Upsert/Delete/CreateCollection/DropCollection の 4 種を
  follower memtable に反映するだけ。**replica 自身の WAL は書かない**
  (fetch した WAL ファイルの追記がその代わり。クラッシュしても
  通常の open と同じリプレイで復元される)

## 4. ストレージ層: follower モード (todo 903)

`Store::open_follower(path)`:

- 通常の open と同じ復旧手順 (manifest 読み込み + WAL リプレイ) を行うが:
  - **書き込み API はすべて `HamaneError::ReadOnlyReplica` を返す**
  - 自動フラッシュ・コンパクション・WAL 書き込みを行わない
  - flock は通常どおり取る (1 ディレクトリ 1 プロセス)
- 追加 API:
  - `apply_wal_frame(bytes)`: fetch した WAL フレームを follower memtable に適用
  - `switch_generation()`: ディスク上の新しい CURRENT/セグメントを開き直し、
    状態を原子的に差し替える (検索中の読者は旧 LiveView を持ち続けてよい —
    Arc なので安全に旧世代を見終えられる)

## 5. サーバ統合 (todo 904)

- primary: `/replication/*` は常に有効 (認証内)。追加設定なし
- replica: `hamane-server --replicate-from <primary-url>` で起動
  - puller スレッドが §3 のループを回す
  - 書き込み系エンドポイント (upsert/delete/create/drop/flush/compact) は
    409 Conflict `{"error": "read-only replica"}`
  - 検索・参照系はそのまま (これが read スケールの本体)
  - `/health` に `{"role": "replica", "lag_wal_bytes": N, "manifest_gen": g}`
    を足して監視可能にする
- 昇格: replica プロセスを止め、`--replicate-from` なしで起動し直す。
  ディスクレイアウトが primary と同一なので通常の open がそのまま通る

## 6. HTTP クライアント

依存を増やさないため std::net の最小 HTTP/1.1 クライアントで実装する
(--healthcheck と同系統。GET のみ、`Content-Length` 前提、リダイレクトなし)。
既知の制限: chunked encoding を返す中間プロキシ越しでは動かない。
v0 は直結 (またはプロキシで buffering off) を前提とし、制限として明記する。

## 7. テスト戦略 (todo 904/905)

- 結合: 同一プロセスに primary Database + HTTP ルーターと replica を立て、
  upsert → ポーリング 1 周 → replica で検索できることを確認
  (フラッシュ跨ぎ / コンパクション跨ぎ / WAL tail のみ、の 3 経路)
- 部分転送: WAL フレームを意図的に途中で切って送り、適用が
  フレーム境界で止まり次回に継続することを確認
- 昇格: replica を通常モードで開き直し、書き込みできること
- 競合: 同期中にコンパクションを走らせ、404 → 再同期で収束すること

## 8. マイルストーン内訳

| todo | 内容 |
|---|---|
| 901 | この設計文書 |
| 902 | primary 側 /replication API (state/manifest/segment/wal) |
| 903 | hamane-storage の follower モード (open_follower / apply_wal_frame / switch_generation) |
| 904 | replica puller + --replicate-from + read-only ハンドリング + 結合テスト |
| 905 | 昇格手順の検証・仕様書 (docs/spec) とベンチ (レプリカで read スケール実測) |
