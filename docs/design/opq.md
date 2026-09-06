# 詳細設計: OPQ (回転付き直積量子化) (M11)

[quantization.md](quantization.md) §0 で M10 の非ゴールとした「直積量子化の
最適化版 (OPQ)」を実装する。M10 の PQ / IVF-PQ のコードブック学習の**前段に
直交回転を挟むだけ**の拡張で、探索経路・ファイル構成・API はほぼそのまま使える。

- Status: Draft v0.1 (2026-09-06)
- 対応タスク: todos/1101〜1104
- 前提: PQ (`hamane-core/src/pq.rs`, `PqCodebook`)、セグメントの量子化ファイル
  (quantization.md §1.4, §3.3)

---

## 0. 動機

PQ はベクトルを `m` 本のサブベクトルに**位置で機械的に分割**し、それぞれ独立に
量子化する。この分割は次のとき大きな誤差を生む:

- **サブベクトル間の分散の偏り**: 分散の大きいサブベクトルも小さいサブベクトルも
  同じ 256 セントロイドを割り当てるため、前者の誤差が支配的になる
- **次元間の相関**: 相関のある次元が別々のサブベクトルに散ると、独立量子化は
  その相関を表現できない (同じビット数でより粗い近似になる)

OPQ は直交行列 `R` (d×d) を学習し、`x' = R x` に回転してから PQ する。直交変換は
L2 距離と内積を保存する:

```
‖R q − R x‖ = ‖q − x‖ ,  ⟨R q, R x⟩ = ⟨q, x⟩
```

したがって**検索の意味は一切変わらず**、量子化誤差だけが下がる。再ランクは
従来どおり `vectors.bin` の生 f32 で行うので、最終スコアも回転の影響を受けない。

### 非ゴール

- パラメトリック OPQ (ガウス仮定の閉形式初期化 = PCA + 分散均等化)。v0 は
  非パラメトリック (交互最適化) のみ。初期値は乱数直交行列 (§1.1)
- 回転付き SQ8。SQ8 は次元ごと独立なので回転の利得がない (むしろ分布が崩れる)
- 学習済み回転行列のセグメント間共有。コードブックと同じくセグメント単位で固定

---

## 1. 定式化と学習

最小化したいのは回転後空間での量子化誤差:

```
min_{R, C}  Σ_i ‖R x_i − q_C(R x_i)‖²      s.t. RᵀR = I
```

`q_C` は PQ の符号化 → 復元。`R` と `C` (コードブック) を交互に最適化する。

### 1.1 交互最適化 (todos/1102)

```
R ← 乱数直交行列 (seed 固定)
repeat T 回:
    Y  ← { R x_i }                      # 回転
    C  ← PqCodebook::train(Y, m, ...)   # 既存の k-means (少ない反復)
    Ŷ  ← { 復元(符号化(y_i)) }           # 量子化後の再構成
    M  ← Ŷᵀ X                           # d×d
    R  ← polar(M)                       # 直交 Procrustes の解
最後に C を通常の反復数で再学習して確定
```

`R` の更新は**直交 Procrustes 問題** `max_R tr(R Xᵀ Ŷ)` の解であり、
`M = Ŷᵀ X` の特異値分解 `M = U S Vᵀ` に対して `R = U Vᵀ`、すなわち `M` の
**直交極因子** (polar factor) に等しい。

**初期値を恒等行列にしてはいけない**: 軸に沿った分布では復元 `Ŷ` も軸に沿うため
`M = Ŷᵀ X` がほぼ対角になり、極因子が恒等行列を返して 1 歩も動かない (恒等は
局所最適)。決定的な乱数直交行列 (Gram-Schmidt 直交化、seed 固定) から始める。
乱数回転はそれ自体が分散を均す強いベースラインでもある。

### 1.2 極分解を Newton 反復で解く

SVD の実装を避けるため、極因子を Newton 反復で直接求める:

```
Q₀ = M
Q_{k+1} = ½ (Q_k + Q_k⁻ᵀ)     →  Q_k → polar(M)
```

二次収束するので 20 反復で十分 (実際は 10 回未満で `‖QᵀQ − I‖` が 1e-6 を切る)。
必要なのは **d×d の逆行列** (Gauss-Jordan、部分ピボット選択) だけで、外部
クレートに依存しない。

- `M` が特異 (低ランク) の場合は逆行列が求まらない。そのときは対角にリッジ
  `ε‖M‖/d` を足して再試行し、失敗するたびにリッジを 10 倍して最大 8 回粘る。
  それでも駄目なら**その反復を捨てて直前の `R` を維持**する (学習を絶対に
  失敗させない = PQ の m 自動決定と同じ方針)
- 出力は必ず直交性を検証する (`‖RᵀR − I‖_max < 1e-3`)。外れたら `R = I` に
  フォールバックし、実質 PQ と同じ挙動にする

### 1.3 計算コスト

1 反復あたり `O(n·d²)` (回転) + `O(n·m·ksub·dsub)` (符号化) + `O(d³)` (極分解)。
PQ 学習をそのまま T 回まわすと M10 比 T 倍になるので、次で抑える:

| パラメータ | 既定 | 意味 |
|---|---|---|
| `OPQ_ITERS` | 8 | 交互最適化の反復数 |
| `OPQ_TRAIN_SAMPLE` | 16384 | 回転学習に使う行数 (PQ 本学習の 65536 とは別) |
| `OPQ_INNER_ITER` | 5 | 反復中の k-means 反復数 (最終学習は 25 = 既定のまま) |

見積り (dim=128, m=32, n=200k): 追加コストは PQ 学習の約 1.6 倍 + d³ 項 (無視可)。
実測は todos/1104 で記録する。

---

## 2. 適用範囲と探索

| 構成 | OPQ の適用 |
|---|---|
| Hnsw + Pq | `vectors_pq.bin` の codes を回転後空間で符号化。探索はクエリを回転して ADC |
| IvfPq + Pq | **粗量子化も残差 PQ も回転後空間**で行う (回転は L2 を保存するので粗セントロイドの意味は不変)。実装上はセグメント構築の入口で回転済みデータを 1 度作り、以降は 1004/1005 と同じ処理 |
| Hnsw + Sq8 / Ivf + None | 非対応 (open 時エラー、§4) |

探索経路の変更は**クエリの回転 1 箇所だけ**:

```
q' = R q  →  既存の build_lut(q') / nprobe_lists(q') / build_list_lut(q')
```

再ランクは `vectors.bin` の生 f32 と生クエリで行うため回転不要。したがって
`collection.rs` の差分は「`opq_view()` があればクエリを回した値を使う」だけで、
候補選択・再ランク・フィルタの構造は M10 のまま変わらない。

---

## 3. オンディスクフォーマット: opq.bin

セグメントディレクトリ内の**任意ファイル**。存在すれば回転あり、無ければ
従来 PQ (M10 で書いたセグメントはそのまま読める = 前方互換)。

```
header (64 B に pad):
  magic[8] = b"HAMANEO\x01"
  dim u32
rotation:
  dim × dim × f32        // 行優先。x' = R x
footer:
  crc32c u32 (header..rotation 全体)
```

- ロード時に `dim` がセグメントと一致すること、`‖RᵀR − I‖_max < 1e-3` を検証。
  壊れていれば `Corrupted`
- `dim=768` で 2.25 MB。セグメントあたり定数サイズなので行数が増えても無視できる
- 回転の適用は `OpqRotation::apply(x, out)` (d² の積和)。クエリ 1 回のみなので
  SIMD 化は当面不要 (dim=768 で ~0.6 M flop = 数十 µs、検索全体では誤差)

---

## 4. StoreOptions への統合

`nprobe` と同じく**独立フィールド**として足す。`Quantization::Pq` にフィールドを
足すと既存の構造体リテラルが全て壊れるため採らない。

```rust
struct StoreOptions {
    // ...既存...
    /// PQ コードブックの前に直交回転を学習する (todo 1102)。
    /// quantization = Pq のときのみ有効
    opq: bool,          // 既定 false
}
```

許可する組み合わせ (quantization.md §5 の 5 通りに OPQ 列を足したもの):

| index | quantization | opq | 書くファイル |
|---|---|---|---|
| Hnsw | None | false | hnsw.bin |
| Hnsw | Sq8 | false | hnsw.bin + vectors_sq8.bin |
| Hnsw | Pq | false / **true** | hnsw.bin + vectors_pq.bin (+ **opq.bin**) |
| Ivf | None | false | ivf.bin |
| IvfPq | Pq | false / **true** | ivfpq.bin (+ **opq.bin**) |

`opq = true` かつ quantization が `Pq` 以外は `open` 時に `InvalidConfig`。

---

## 5. コード配置

- `hamane-core/src/opq.rs` (新規): 行列ユーティリティ (積・転置・逆行列)、
  極分解 Newton、`OpqRotation` (train / apply / from_matrix / matrix)。
  pq.rs と同じく**ストレージ非依存の純粋な数値処理**
- `hamane-storage/src/segment.rs`: `opq.bin` の書き出しとロード、`OpqView`。
  `write_pq` / `write_ivfpq` の入口で回転済みデータを作る
- `hamane/src/collection.rs`: クエリ回転 (PQ / IVF-PQ 経路の LUT 構築前)
- `hamane-index`: 変更なし (M10 と同じ理由)

---

## 6. テストと検証

- **直交性**: 学習した `R` が `RᵀR ≈ I`。特異な `M`・全点同一などの縮退入力でも
  panic せず `R = I` にフォールバック
- **誤差**: 次元ごとに分散が偏り、かつ次元間に相関のある人工データで
  **OPQ の再構成 MSE < PQ の再構成 MSE** (同じ m・同じ seed、決定的)
- **ラウンドトリップ**: opq.bin 書き出し → mmap ロード → CRC・直交性検証
- **再現率**: `Hnsw+Pq+opq` / `IvfPq+opq` で recall@10 ≥ 0.95 (再ランクあり)
- **互換**: opq.bin の無い M10 セグメントが従来どおり読める。`opq=false` で
  全既存テストが green
- **ベンチ (todos/1104)**: SIFT で PQ vs OPQ の recall / QPS / 構築時間を対比し
  docs/benchmarks.md に追記
