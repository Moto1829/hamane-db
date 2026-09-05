# 詳細設計: 量子化とクラスタリング索引 (M10)

DESIGN.md §4「将来拡張」の詳細化。直積量子化 (PQ)、転置ファイル (IVF)、
および両者を組み合わせた IVF-PQ を規定する。既存のスカラー量子化 (SQ8,
todos/602) と HNSW (docs/design/index.md) の枠組みを再利用し、フォーマット互換を
保ったまま opt-in で追加する。

- Status: Draft v0.1 (2026-08-26)
- 対応タスク: todos/1001〜1006
- 前提: SQ8 (`hamane-core/src/sq8.rs`, `Sq8View`)、HNSW (`search_hnsw_by`)、
  セグメントフォーマット (docs/design/storage.md §3)

---

## 0. スコープと位置づけ

SQ8 は f32 を次元ごと 1 バイトに落とす (1/4 圧縮)。数千万〜億件規模では
これでも足りず、より強い圧縮 (16〜64x) と、全セグメント全走査を避ける粗い
枝刈りが欲しい。M10 はこの 2 軸を扱う。

| 手法 | 圧縮率 (dim=768, f32=3072B 基準) | 効果 | 探索 |
|---|---|---|---|
| SQ8 (既存) | 4x (768 B) | メモリ削減 + 整数距離 | HNSW を SQ8 距離で辿り f32 再ランク |
| **PQ** | m=96, nbits=8 で 32x (96 B) | 強いメモリ削減 | HNSW を ADC 距離で辿り f32 再ランク |
| **IVF** | 圧縮なし | セグメント内を nprobe クラスタに枝刈り | HNSW の代替 (排他) |
| **IVF-PQ** | PQ と同じ | 枝刈り + 強圧縮 | 転置リスト × ADC |

いずれも SQ8 と同じく、元の `vectors.bin` を必ず残す (f32 再ランク・再構築用)。
量子化・索引ファイルが無ければ従来動作にフォールバックする。既定は全て off。

### 非ゴール (M10 では扱わない)

- 直積量子化の最適化版 (OPQ = 回転付き PQ)。PQ の精度が不足したときの将来拡張
- グラフと転置の併用 (IVF の各リスト内 HNSW など)。IVF は Flat スキャンで確定
- 量子化コードブックの動的再学習。コードブックはセグメント構築時に固定 (不変)

---

## 1. PQ (直積量子化)

### 1.1 構造

dim 次元ベクトルを `m` 本のサブベクトル (各 `dsub = dim / m` 次元) に分割し、
サブベクトルごとに独立した `k* = 2^nbits` 個のセントロイド (コードブック) を持つ。
1 本のベクトルは `m` 個のセントロイド ID の列 (= `m × nbits` ビット) で表す。

| パラメータ | 既定 | 意味 |
|---|---|---|
| `m` | 次元依存 (§4) | サブベクトル数。`dim % m == 0` を要求 |
| `nbits` | 8 | サブコード幅。8 = 1 サブベクトル 1 バイト・k*=256 |
| `k*` | 256 | サブコードブックのセントロイド数 (= 2^nbits) |

nbits は当面 8 のみ実装する (コードが `m` バイトに素直に収まり、LUT が
サブベクトルあたり 256 エントリで済む)。4/6 は将来拡張。

### 1.2 コードブック学習 (todos/1002)

サブベクトルごとに k-means (Lloyd) を回す。

- 入力: セグメント構築時の全 f32 ベクトル (フラッシュ / コンパクションの行集合)。
  件数が多い場合は `train_sample`(既定 65536) 行をサブサンプリングして学習
- 初期化: k-means++ (seed 固定。フラッシュ時は seg_id を seed に混ぜて決定的に)
- 反復: `max_iter`(既定 25) か重心移動が閾値未満で停止
- 出力: `m × k* × dsub` の f32 セントロイド + 各行の `m` バイトコード
- 距離は L2 で学習する (dot/cosine も L2 空間での近さで代用。cosine は既に
  正規化済みなので L2 最近傍 = 角度最近傍)

空クラスタは「最大クラスタから最遠点を割り当てる」標準の再割り当てで回避。

### 1.3 ADC 距離 (Asymmetric Distance Computation)

クエリは量子化せず f32 のまま、DB 側コードとの距離を推定する。検索開始時に
クエリのサブベクトルと全セントロイドの距離を先に計算し LUT に載せる:

```
lut[j][c] = distance(query_sub[j], centroid[j][c])   // j: 0..m, c: 0..k*
```

- **L2²**: `lut[j][c] = Σ_d (q[j][d] − centroid[j][c][d])²`。行の推定距離は
  `Σ_j lut[j][code[row][j]]` (m 回の表引き加算)
- **dot**: `lut[j][c] = Σ_d q[j][d] · centroid[j][c][d])`。推定内積は
  `Σ_j lut[j][code[row][j]]`。`distance_key` は既存同様に符号反転
- LUT 構築は `O(m · k* · dsub) = O(k* · dim)`。行あたりの評価は `O(m)`

この `dist: |row: u32| -> f32` を **既存の `search_hnsw_by` にそのまま渡す**。
SQ8 と同一の 2 段階検索: HNSW を ADC で `k × PQ_RERANK`(既定 SQ8 と揃えて 4)
件まで辿り、`vectors.bin` の f32 で再ランクして top-k を返す。

### 1.4 オンディスクフォーマット: vectors_pq.bin

セグメントディレクトリ内。SQ8 の `vectors_sq8.bin` と同じ扱い (任意ファイル、
無ければ非 PQ 動作)。magic は共通規約に従い `b"HAMANEP\x01"`。

```
header (64 B に pad):
  magic[8] = b"HAMANEP\x01"
  dim u32, count u64
  m u32, nbits u8, ksub u32   // ksub = 2^nbits
codebook:
  m × ksub × dsub × f32       // サブベクトル j → セントロイド c → dsub 次元
codes:
  count × m × u8              // 行 → m サブコード (nbits=8 前提)
footer:
  crc32c u32 (header..codes 全体)
```

- `PqView` は mmap スライスへの参照のみ保持。`build_lut(query, metric)` で
  LUT を作り、`distance_key(lut, row)` で表引き加算
- `dim % m != 0` はビルド時エラー。codebook 領域は f32 なので 4B アライン

---

## 2. IVF (転置ファイル)

### 2.1 構造

セグメント内の行を `nlist` 個の粗クラスタ (coarse centroid) に割り当て、
クラスタごとに行番号の転置リストを持つ。検索時はクエリに近い `nprobe` 個の
クラスタだけを走査し、残りを枝刈りする。

| パラメータ | 既定 | 意味 |
|---|---|---|
| `nlist` | ≈ `sqrt(count)` に丸め (§4) | 粗クラスタ数 |
| `nprobe` | 8 (クエリごとに上書き可) | 探索するクラスタ数 |

### 2.2 HNSW との関係 — 排他

HNSW も IVF も「全走査を避ける枝刈り」機構であり、同一セグメントに両方を
持たせる意味は薄い (二重の近似で再現率が落ちる)。**セグメント単位でどちらか
一方**とする:

- `StoreOptions.index = Hnsw | Ivf | IvfPq`(既定 `Hnsw`)
- IVF 系を選んだセグメントは `hnsw.bin` を書かず、`ivf.bin` を書く
- 検索ソースの分岐 (`collection.rs`) に IVF 経路を追加。memtable は従来どおり
  Flat (件数が小さくクラスタ化が無意味)

将来 IVF リスト内を HNSW で辿る構成は考えうるが M10 非ゴール (§0)。

### 2.3 探索 (todos/1004)

1. クエリと `nlist` 粗セントロイドの距離を全計算し、近い順 `nprobe` 個を選ぶ
   (nlist は sqrt オーダなので全計算で十分安価)
2. 選んだリストの行を Flat スキャンで距離計算 (live マスク・フィルタ適用)
3. 全 probe 分をマージして上位 k

粗セントロイド学習は §1.2 と同じ k-means を **全次元** (dim 次元) に対して
`nlist` クラスタで回す (コードブック学習の特殊形として実装を共有)。

### 2.4 オンディスクフォーマット: ivf.bin

```
header (64 B に pad):
  magic[8] = b"HAMANEC\x01"   // Coarse
  dim u32, count u64, nlist u32
centroids:
  nlist × dim × f32
CSR 転置リスト (list 0..nlist):
  offsets:  (nlist+1) × u64   // list l の行は entries[off[l]..off[l+1]]
  entries:  count × u32       // 行番号 (list ごとに昇順)
footer:
  crc32c u32
```

行→リストの逆引きは検索に不要 (リスト→行だけ辿る)。newest-wins の live 判定は
既存の `LiveView` をそのまま使う (行番号ベースなので IVF でも不変)。

---

## 3. IVF-PQ (residual PQ)

IVF の枝刈りと PQ の圧縮を組み合わせる。各行を、所属クラスタの粗セントロイドを
引いた **残差ベクトル** `r = x − centroid[list]` として PQ 符号化する。残差は
分布が原点付近に集中するため、同じ `m` でも生ベクトル PQ より量子化誤差が小さい。

### 3.1 学習と符号化 (todos/1005)

1. §2.3 の粗 k-means で `nlist` セントロイドと各行の所属リストを得る
2. 全行の残差 `r_i` を集めて §1.2 の PQ コードブックを 1 組学習
   (リスト非依存の共有コードブック。IVFADC の標準構成)
3. 各行を残差の PQ コードで符号化

### 3.2 探索

1. §2.3 で `nprobe` リストを選ぶ
2. 各 probe リスト `l` について、クエリ残差 `q − centroid[l]` で ADC LUT を
   構築 (probe ごとに LUT を作り直す。nprobe は小さいので許容)
3. リスト内の行を ADC 表引きで評価、`k × PQ_RERANK` 件を候補に
4. 全 probe をマージ → `vectors.bin` f32 で再ランク → top-k

### 3.3 フォーマット: ivfpq.bin

`ivf.bin` (§2.4) の CSR に、entries と並べて残差 PQ コードを持たせた統合形。

```
header (64 B に pad):
  magic[8] = b"HAMANEG\x01"    // SQ8 が既に Q を使うため G (IVF-PQ)
  dim u32, count u64, nlist u32
  m u32, nbits u8, ksub u32
coarse centroids:  nlist × dim × f32
pq codebook:       m × ksub × dsub × f32   // 残差空間で学習
CSR:
  offsets:  (nlist+1) × u64
  entries:  count × u32          // 行番号 (list ごと昇順)
  codes:    count × m × u8        // entries と同順の残差 PQ コード
footer: crc32c u32
```

`PqView` のコードブック引き部分を再利用し、`IvfPqView` は「coarse 選択 →
リストごとの残差 LUT → 表引き」を担う。

---

## 4. パラメータ既定と自動決定

- `m`: `dim` の約数のうち `dim/m ≥ 4` かつ `m ≤ 96` を満たす最大値を既定に
  (dim=768 → m=96・dsub=8、dim=128 → m=32・dsub=4、dim=100 → m=25・dsub=4)。
  約数でない場合はユーザ指定必須 (ビルド時エラーで明示)
- `nlist`: `clamp(round(sqrt(count)), 16, 65536)`。count が `nlist × 39` 未満
  (k-means の目安 39 点/クラスタ) なら nlist を縮小
- `nbits = 8` 固定 (§1.1)
- `train_sample = 65536`, `max_iter = 25`, `PQ_RERANK = 4`, `nprobe = 8`

小さいセグメント (`count < min_train`, 既定 `nlist × 39` や PQ の `ksub × 39`)
では学習が不安定なので量子化・IVF を書かず Flat/HNSW にフォールバックする
(SQ8 の `hnsw_min_rows` と同じ発想)。

---

## 5. StoreOptions への統合

既存の `sq8: bool` と同じ実行時オプション面に追加する (manifest には永続化
しない。フォーマットはファイルの有無で自己記述的)。

```rust
enum IndexKind { Hnsw, Ivf, IvfPq }   // 既定 Hnsw

struct Quantization {                  // 既定 None
    Sq8,                               // 既存
    Pq { m: Option<usize>, nbits: u8 },
}

struct StoreOptions {
    // ...既存...
    index: IndexKind,
    quantization: Option<Quantization>,  // Sq8 は sq8:bool を吸収
    ivf: IvfOptions,                     // nlist, nprobe, train_sample...
}
```

組み合わせの意味:

| index | quantization | 書くファイル | 検索 |
|---|---|---|---|
| Hnsw | None | hnsw.bin | f32 HNSW (現行) |
| Hnsw | Sq8 | hnsw.bin + vectors_sq8.bin | SQ8 HNSW + 再ランク (現行) |
| Hnsw | Pq | hnsw.bin + vectors_pq.bin | ADC HNSW + 再ランク |
| Ivf | None | ivf.bin | IVF Flat |
| IvfPq | Pq | ivfpq.bin | IVF + 残差 ADC + 再ランク |

`Ivf + Pq`(非残差) や `IvfPq + Sq8` のような冗長・無意味な組は open 時に
エラーにする (表の 5 行のみ許可)。

---

## 6. コード配置

- `hamane-core/src/pq.rs`: k-means (共有)、`PqCodebook`(学習・符号化・LUT)。
  純粋な数値処理でストレージ非依存 (SQ8 が `sq8.rs` にあるのと対称)
- `hamane-storage/src/segment.rs`: `PqView` / `IvfView` / `IvfPqView` と
  ビルド時の書き出し (`SegmentWriter` の spec 拡張)
- `hamane-index`: 変更なし (`search_hnsw_by` が距離関数を受けるため PQ は
  distance クロージャの差し替えだけ)。IVF 探索は storage 側に閉じる
- `hamane/src/collection.rs`: 検索ソース分岐に PQ / IVF 経路を追加

---

## 7. テストと検証

- **数値**: ADC 推定距離 vs f32 実距離の相対誤差、残差 PQ が生 PQ より低誤差
  であること (決定的 seed)
- **再現率**: ランダム + クラスタ混合データで各構成の recall@10 を Flat 正解と
  比較。**再ランクあり前提で recall@10 ≥ 0.95 を維持** (SQ8 と同基準)
- **ラウンドトリップ**: 各 `*.bin` の書き出し→ mmap ロード→ CRC 検証
- **フォールバック**: 量子化/IVF ファイルが無い・小セグメントで従来動作、
  全既存テストが green (off の互換保証)
- **ベンチ (todos/1006)**: SIFT1M で構成ごとの recall / QPS / メモリ・ディスク
  サイズを計測し docs/benchmarks.md に記録。特に PQ のメモリ削減と IVF-PQ の
  枝刈り効果を SQ8・素 HNSW と対比する
