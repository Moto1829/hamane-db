# 705: hamane-server の API キー認証

- Status: DONE (2026-07-15)
- Milestone: M7
- Depends: 603
- Design: docs/spec (limits.md「認証・TLS はない」の認証側を解消)

## ゴール

hamane-server を直接公開する場合の最低限の保護として、静的 API キー認証を
提供する。TLS は引き続きスコープ外 (リバースプロキシ前提)。

## やること

- [ ] 起動オプション `--api-key <KEY>` と環境変数 `HAMANE_API_KEY`
      (フラグ優先)。未指定なら認証なし (ローカル開発向け、起動時に警告)
- [ ] ミドルウェアで全エンドポイントを保護:
      `Authorization: Bearer <key>` または `X-Api-Key: <key>` を受理
- [ ] キー比較は定数時間 (タイミング攻撃対策)
- [ ] 不一致・欠落は 401 (`{"error": "unauthorized"}`)
- [ ] テスト: キーあり (401 / Bearer / X-Api-Key の 200) とキーなし (素通し)

## 完了条件

- 結合テスト green
- docs/spec (limits.md) とサーバの rustdoc・README を更新
