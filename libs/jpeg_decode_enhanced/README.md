# jpeg_decode_enhanced

ESP32-P4 のハードウェア JPEG デコーダを拡張する ESP-IDF コンポーネント。
IDF 標準の `jpeg_decoder_process()` を置き換え、次の 2 つを提供する。

- **Layer 1: ストリップ分割デコード** (`jpeg_decode_enhanced.h`) —
  デコード結果を全画面バッファではなく「ストリップ」(16 行単位の帯) として
  利用者が渡す 1〜2 枚のバッファへ 2D-DMA で直接書き込み、1 ストリップ完了
  ごとにコールバックで下流へ渡す。中間バッファが小さくなり、Internal SRAM に
  置けば PSRAM 帯域のボトルネックを回避できる。フルレンジ YUV→RGB 変換にも対応。
- **Layer 2: PPA パイプライン** (`jpeg_ppa_pipeline.h`) — Layer 1 の
  ストリップを PPA SRM (scale / rotate / mirror) へ流し込み、デコードと
  並行して出力フレームバッファへ合成する高レベル API。PPA へは IDF の
  `ppa_do_scale_rotate_mirror()` を通さず、ISR から 2D-DMA へ直接投入する。

全 API は C 言語 (opaque handle + `esp_err_t`)。

## なぜ存在するか

JPEG コーデック単体は 1280×720 を 60fps でデコードできるが、IDF 標準の

```
jpeg_decoder_process → PSRAM → PPA SRM → PSRAM フレームバッファ
```

という経路では PSRAM 帯域が律速になり実測 ~20fps で頭打ちになる。
中間ラスタを Internal SRAM のストリップバッファに置き、JPEG デコードと
PPA 処理をストリップ単位でパイプライン化すると PSRAM の読み戻しが消え、
カメラ律速の 30fps に戻る (M5Stack Tab5 + UVC カメラで実測)。

```
JPEG codec ──TX: PSRAM 上の JPEG ストリーム──┐
                                             ▼
                            2D-DMA RX (ストリップごとの連結ディスクリプタ)
                                             │
                                             ▼
                    ストリップバッファ (利用者確保 1〜2 枚, SRAM or PSRAM)
                                             │
                                             ▼  on_strip_done (ISR): PPA が空いていれば投入
                                       PPA SRM (2D-DMA 直接投入)
                                             │  PPA 完了 (ISR): release → 次のストリップを投入
                                             ▼
                                  出力フレームバッファ (PSRAM)
```

## 使い方

### Layer 2: JPEG → 変換 → フレームバッファ (一番よく使う形)

```c
#include "jpeg_ppa_pipeline.h"

// 1280 幅 × 16 行 × RGB888 を 2 枚 (≒ 120 KiB)
size_t sz = 16 * 1280 * 3;
void *b0 = heap_caps_aligned_calloc(64, 1, sz, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL);
void *b1 = heap_caps_aligned_calloc(64, 1, sz, MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL);

jpeg_ppa_pipeline_cfg_t cfg = {
    .strip_bufs       = { b0, b1 },                // b1 = NULL なら 1 枚運用
    .strip_buf_size   = sz,
    .strip_color_mode = PPA_SRM_COLOR_MODE_RGB888, // 中間フォーマット
    .rgb_order        = JPEG_DEC_RGB_ELEMENT_ORDER_BGR,
    .conv_std         = JPEG_YUV_RGB_CONV_STD_BT601,
    .yuv_full_range   = true,                      // MJPEG はフルレンジ
    .timeout_ms       = 0,                         // 0 = 200 ms
};
jpeg_ppa_pipeline_handle_t pipe;
ESP_ERROR_CHECK(jpeg_ppa_pipeline_new(&cfg, &pipe));

// 変換パラメータは毎フレーム変えられる
jpeg_ppa_transform_t t = {
    .rotation = PPA_SRM_ROTATION_ANGLE_90,
    .scale_x  = 1.0f,
    .scale_y  = 1.0f,
    // .mirror_x / .mirror_y / .rgb_swap / .byte_swap
    // .in_crop = {x, y, w, h}   入力空間クロップ (w/h=0 で全体)
    // .out_offset_x / .out_offset_y  出力先の左上座標
    // .out_clip = {x, y, w, h}  出力空間のクリップ (w/h=0 でなし)
};
jpeg_ppa_output_t out = {
    .buffer     = framebuffer,
    .pic_w      = 720,
    .pic_h      = 1280,
    .color_mode = PPA_SRM_COLOR_MODE_RGB565,
};
ESP_ERROR_CHECK(jpeg_ppa_pipeline_process(pipe, jpeg, jpeg_size, &out, &t, NULL));
// 戻った時点で全ストリップの PPA 書き込みまで完了している
```

リソース (ストリップバッファ・JPEG エンジン・PPA クライアント) は
`new` 時に固定。**回転・スケール・ミラー・クロップ・出力位置は
`process()` ごとに自由に変えられる**。入力 JPEG のサイズも毎フレーム
異なってよく (ヘッダから自動検出)、ストリップ高さはフレームごとに決まる。
ストリップバッファは `del` するまで保持し、`del` 後に利用者が解放する。

`process()` の間は PPA SRM エンジンを占有する。同じアプリ内の
`ppa_do_scale_rotate_mirror()` (ディスプレイの回転など) はその間待たされ、
最大 1 フレームぶん遅れる。PPA の blend / fill は影響を受けない
(2D-DMA チャネルが空くまで待つだけ)。

出力先と同じサイズ・無回転・等倍で PPA が要らないフレームは、
`jpeg_ppa_pipeline_get_decoder()` で内部の Layer 1 ハンドルを取り出し、
`jpeg_enh_decoder_process()` で出力バッファへ直接デコードできる。PPA を
通さない分速く、JPEG エンジンを 2 つ持つ必要もない。`process()` と同時に
呼ばないこと。

### Layer 1: ストリップを自分で消費する

```c
#include "jpeg_decode_enhanced.h"

static bool on_strip(const jpeg_enh_strip_event_t *e, void *ctx) {
    // ISR コンテキスト。e->buffer に e->rows 行ぶんの画素が入っている。
    // queue 等で task に渡し、処理が終わったら必ず
    // jpeg_enh_strip_decoder_release_strip(dec, e->strip_idx) を呼ぶこと。
    return false;
}

jpeg_enh_strip_decoder_cfg_t cfg = {
    .decode = {
        .output_format  = JPEG_DECODE_OUT_FORMAT_RGB565,
        .rgb_order      = JPEG_DEC_RGB_ELEMENT_ORDER_BGR,
        .conv_std       = JPEG_YUV_RGB_CONV_STD_BT601,
        .yuv_full_range = true,
    },
    .strip_bufs     = { b0, b1 },
    .strip_buf_size = sz,
    .on_strip_done  = on_strip,
};
jpeg_enh_strip_decoder_handle_t dec;
ESP_ERROR_CHECK(jpeg_enh_strip_decoder_new(&cfg, &dec));

jpeg_enh_frame_info_t info;
ESP_ERROR_CHECK(jpeg_enh_strip_decoder_process(dec, jpeg, jpeg_size, &info));
```

- `on_frame_start` (任意) はヘッダ解析直後・DMA 開始前に呼び出し元 task の
  コンテキストで呼ばれる。フレームごとの前準備や検証に使える
  (ESP_OK 以外を返すとそのフレームを中止)。
- ストリップの消費が終わるたびに `release_strip()` を呼ぶこと。これが
  バッファ再利用のバックプレッシャーそのもの (下の内部構造を参照)。
- CPU でストリップを読む場合は、読む前に `sync_strip_for_cpu()` を呼ぶ。

### 全画面一発デコード (PPA なし、フルレンジだけ欲しいとき)

```c
size_t buf_size;
void *buf = jpeg_alloc_decoder_mem(1280 * 720 * 2,
        &(jpeg_decode_memory_alloc_cfg_t){ .buffer_direction = JPEG_DEC_ALLOC_OUTPUT_BUFFER },
        &buf_size);

jpeg_enh_frame_info_t info;
ESP_ERROR_CHECK(jpeg_enh_decoder_process(dec, jpeg, jpeg_size, buf, buf_size, &info));
// IDF の jpeg_decoder_process() 相当 + full range 対応。
// strip_bufs を渡さずに作った handle でも使える。
```

出力バッファは IDF の `jpeg_decoder_process()` と同じアライメント規則
(`jpeg_alloc_decoder_mem()` を使うのが確実)。PSRAM バッファのキャッシュ
無効化はコンポーネント側で行う。

## 設定リファレンス

### ストリップバッファ

| フィールド | 意味 |
|---|---|
| `strip_bufs[0]` | ストリップバッファ 1 枚目。NULL = ストリップモードなし (全画面一発デコード専用) |
| `strip_bufs[1]` | 2 枚目。NULL = 1 枚運用 (デコードと消費が交互になり遅い) |
| `strip_buf_size` | 各バッファのバイト数 (2 枚とも同じ) |

- バッファは 2D-DMA からアクセスできること (Internal の DMA 領域か PSRAM)。
  2 枚の種類は違ってよい。Internal が高速、PSRAM は SRAM 節約用。
- 先頭は 64 バイト (またはキャッシュライン) 境界に揃えること。満たさなければ
  `new()` が `ESP_ERR_INVALID_ARG`。サイズ末尾の端数は使われない。

ストリップ高さはフレームごとに次で決まる:

1. 1 枚に入る行数 (`strip_buf_size ÷ (pic_w × bpp)`) を **16 の倍数に切り下げ**
2. 上限は「画像高 ÷ 枚数」を 16 に切り上げた値 (2 枚でフレームを分担させるため)
3. 16 行未満になる幅のフレームは `ESP_ERR_INVALID_SIZE`

必要なサイズの目安は `16 × 最大幅 × bpp ÷ 8` (1280 幅 RGB888 なら 60 KiB/枚)。
余裕を持たせるとストリップが高くなり PPA 呼び出しが減る分少し速い。

16 行単位なのは PPA SRM の都合 (下のチューニング指針)。MCU 高
(YUV444 / YUV422 / GRAY = 8、YUV420 = 16) はいずれも 16 の約数なので、
ストリップ境界は常に MCU 行境界にも乗る。

ストリップ境界が MCU 行境界に乗る制約は**ハードウェア由来で解消不可**
(2D-DMA の reorder がサンプリング依存のマクロブロック単位で動き、
1 MCU 行を 2 つのバッファに跨がせられない)。それ以外は柔軟:

- 画像高がストリップ高で割り切れなくてよい (最終ストリップが短くなる)
- 画像高が MCU 整列でなくてよい (パディング込みでデコードされ、
  strip event の `rows` (有効行) / `padded_rows` (実書き込み行) で区別。
  Layer 2 は有効行だけを PPA に渡すので自動でクロップされる)

### 色変換

| フィールド | 意味 |
|---|---|
| `conv_std` | BT601 / BT709 (YUV→RGB 変換規格) |
| `yuv_full_range` | **false** = IDF 標準の limited range 行列 (互換)。**true** = JFIF フルレンジ行列に差し替え。JPEG/MJPEG は事実上すべてフルレンジなので、画が眠い場合はまずこれを疑う |

フルレンジ行列は BT601 / BT709 両方を内蔵。実装は IDF の CSC 設定
(mux/scramble) をそのまま使い、2D-DMA の行列レジスタだけを上書きする方式。

### Layer 2 transform の制約

- PPA のスケールは **1/16 刻みに量子化**される (YUV420 出力時は 1/8 刻み)。
- 各ストリップは独立した PPA ブロックとして処理されるため、
  **ストリップ内部境界 × 量子化後 scale_y が整数**になる必要がある。
  `process()` がフレームごとに検証し、満たさない場合は
  `ESP_ERR_INVALID_ARG` で拒否する。ストリップは常に 16 行の倍数なので、
  失敗するのは `in_crop.y × scale_y` が整数にならない場合だけ。
- 量子化の副作用として、スケール端数によっては出力の端に数 px の
  未描画帯が出る (例: 600 行 × 1.2 → 量子化 1.1875 → 712 行 ≠ 720)。
- `out_clip` を指定すると、その矩形の外には一切書かない。中は clip なしと
  同じ画素になるが、境界をまたぐソース画素は描かないため、境界沿いに
  最大「スケール後の 1 ソース画素」未満 (YUV420 ストリップは偶数丸めで
  2 ソース画素) の未描画帯が残る。strip の並ぶ軸 (入力 y) はストリップごとに、
  直交する軸 (入力 x) はフレームごとに入力範囲を絞るので、clip 外の
  ストリップは PPA に渡さない (デコード量は変わらない)。clip で途中から
  始まるストリップは次のストリップ側の境界に揃えて置くので、内部に隙間は
  できない。clip なしのときの出力は従来とビット一致 (`test/run.sh` で
  HEAD と比較)、ISR で増えるのは比較数個だけ。YUV420 出力とは併用できない
  (`ESP_ERR_NOT_SUPPORTED`)。
- `mirror_x/y` の配置計算は「PPA のミラーは出力空間で作用する」前提。
  実機未検証 (要確認) なので、初使用時は向きを確認すること。

## ストリップバッファとキャッシュ

ESP32-P4 では **PSRAM だけでなく Internal SRAM もキャッシュ越し**
(キャッシュライン 64 バイト)。キャッシュ整合性はコンポーネント側で処理する:

- `new()` で各バッファを C2M + invalidate してダーティラインをパージ
  (これを怠ると PPA ドライバの入力 writeback が DMA 書き込みを上書きする)
- CPU でストリップを読む消費者は、読む前に
  `jpeg_enh_strip_decoder_sync_strip_for_cpu()` を呼ぶ (M2C invalidate)
- PPA など DMA 消費者はそのままでよい

なお PSRAM バッファでは「PSRAM 書き→PSRAM 読み」の往復が復活するので
高速化効果はない。SRAM 節約が目的のときに使う。

## エラーと診断

- **消費側の停滞**: 消費側が遅いと DMA はチェーン終端で待つだけで、
  遅れ自体はエラーにならない (実測: ストリップごとに 20ms 遅らせても成功)。
  ただしフレーム全体が `timeout_ms` (既定 200ms) を超えると `ESP_ERR_TIMEOUT`
  になり、ログに `strip consumer stalled after N/M strips` と出る。
- 壊れた JPEG (ヘッダ破損など) やバッファに 16 行入らない幅のフレームは
  DMA 開始前に `ESP_ERR_INVALID_STATE` / `ESP_ERR_INVALID_SIZE` で弾く。
- Layer 2 はデコードエラー時に未処理ストリップを精算し、PPA に投入済みの
  1 本の完了を待ってから返るので、次の `process()` はクリーンな状態から
  始まる。PPA 個別ストリップのエラーも `process()` の戻り値に伝播する。
- デコード後 `timeout_ms` 以内に PPA が終わらなければ投入中の 2D-DMA
  トランザクションを強制終了して `ESP_ERR_TIMEOUT`。強制終了できなかった
  (2D-DMA のキューに残っていた) 場合、以降の `process()` は
  `ESP_ERR_INVALID_STATE` を返す。

## チューニング指針 (Tab5 / ESP32-P4 rev1.0 での実測)

1280×720 YUV422 → RGB888 ストリップ → PPA 90° 回転 → 720×1280 RGB565 (PSRAM):

| 構成 | SRAM | 1 フレーム |
|---|---|---|
| 16 行 × 2 枚 (Internal) | 120 KiB | 16.4 ms (IDF の PPA API 経由だと 18.4 ms) |
| 16 行 × 1 枚 | 60 KiB | 28.3 ms (同 30.5 ms) |
| 16 行 × 5 本 (旧実装の推奨, IDF の PPA API 経由) | 300 KiB | 17.4 ms |
| 32 行 × 2 枚 (同上) | 240 KiB | 15.8 ms |

- **3 枚以上にしても速くならない**。律速は PPA (1 ストリップ ≒ 300 µs + ソフト処理、
  JPEG は ≒ 270 µs) で、2 枚あればデコードと PPA の並列化は効き切る。
- **PPA SRM は 16 行単位のマクロブロックで処理する** (rev1.x は 16×16 固定)。
  4〜16 行はどれも ≒ 300 µs、17〜32 行は ≒ 590 µs (1280 幅)。8 行ストリップは
  PPA の仕事量が 16 行の倍になり 34 ms/フレームに落ちる。rev3 以降は
  マクロブロックの既定が 32×32 なので要再計測。
- `ppa_do_scale_rotate_mirror()` 1 回のソフト処理 (検証・msync・キュー・
  タスク起床・2D-DMA 接続) は ≒ 60 µs。Layer 2 の直接投入では検証と
  出力 msync をフレーム 1 回にまとめ、ISR から連鎖投入するので ≒ 23 µs
  (残りは 2D-DMA のチャネル確保と接続)。
- 中間を RGB565 にすると速度は同じで、出力が RGB565 かつ等倍なら
  RGB888 中間と出力がビット一致する (メモリは 2/3)。

## 内部構造 (変更する人向け)

苦労して学んだ 2D-DMA の事実。これを知らずに触ると壊れる:

1. **owner ビットによるバックプレッシャーは JPEG-RX では機能しない**。
   `owner=CPU` + owner チェック有効は `RX_DSCR_ERROR` で transaction が
   終了する (pause しない)。
2. **チェーン終端 (`next=NULL`) では RX が停止して待つ**。`dma2d_append()` で
   再開する (IDF v6.1 で実測。消費側がストリップごとに 20ms 遅れても
   出力はビット一致)。
3. バックプレッシャーは **`dma2d_append()` による動的チェーン延長**。
   最初にバッファ枚数分だけリンクし、消費者が strip `i` を
   `release_strip()` したタイミングで strip `i + 枚数` の descriptor を
   書き込んでスプライスし append する。
   - **RX descriptor は 3 個のプールを使い回す** (strip `i` → `pool[i % 3]`)。
     プールは **枚数 + 1 個以上必要**。1 枚運用でプール 1 個にすると descriptor が
     自分自身を指し、dma2d ISR の `dma2d_ll_rx_is_fsm_idle` assert で落ちる
     (実測)。処理済み descriptor は RX が owner=CPU を書き戻すので、
     再利用時は全フィールドを書き直す。
4. **全ディスクリプタ `suc_eof=0`**。EOF は JPEG ハードのフレーム完了信号から
   ブリッジが生成する。途中の descriptor に `suc_eof=1` を立てると偽 EOF で
   dma2d ISR の `dma2d_ll_rx_is_fsm_idle` assert を踏む。
5. `on_recv_eof` では **engine の `evt_queue` に `JPEG_DMA2D_RX_EOF` を
   post する** (IDF の `jpeg_rx_eof` と同じ)。セマフォだけでは wait ループが
   解けず decode timeout になる。
6. `on_desc_done` の event data には descriptor アドレスが入らない。
   ストリップ番号は順序保証を前提にカウンタで追跡する。
7. **`on_desc_empty` を登録してはいけない**。登録すると DESC_EMPTY 割り込みが
   新規に有効化され、dma2d ドライバはこのビットでチャネル解放 + FSM idle
   assert まで行う。正常フレーム終端で raw ビットが立つかは未検証のため、
   有効化は全フレームを壊すリスクがある。消費側の停滞はタイムアウト時に
   `isr_next_strip == chain_tail + 1` (リンク済み descriptor を全消費して
   未スプライスの所で停止) というシグネチャで事後診断している。
8. **ISR から実行される経路 (Layer 2 のストリップ投入、`release_strip()`、
   descriptor 書き込み) は IRAM に置く**。flash 上に置くと、2D-DMA ISR 内で
   キャッシュラインの境界をまたいだ命令フェッチが `Illegal instruction` で
   落ちることがあった (ESP32-P4 rev1.0 / IDF v6.1、数百フレームに 1 回)。
   IDF の 2D-DMA 操作関数は `CONFIG_DMA2D_OPERATION_FUNC_IN_IRAM` 無効
   (flash 配置) のままでも再現しなかった。
9. `release_strip()` のスプライス + append は `frame_active` フラグで
   ゲートしたスピンロック内で行う。エラーパスでは `dma2d_force_end` の前に
   ゲートを閉じるので、遅れてきた release が解放済み (他用途に再割当て
   されたかもしれない) チャネルを叩くことはない。正常フレームでは EOF 時点で
   全スプライトが完了済みなのでゲートの影響はない。

### PPA SRM 直接投入 (`src/ppa_srm_fast.c`)

`ppa_srm_transaction_on_picked()` と同じレジスタ設定 (YUV444 入力の 2D-DMA CSC、
DIG-734 回避の `bypass_mb_order` を含む) を行うが、フレーム単位で
検証・倍率の量子化・dscr-port ブロックサイズを済ませておき、ストリップごとには
descriptor 2 個の書き込みと `dma2d_enqueue()` だけを行う。

- 他の SRM クライアントとの排他は **PPA ドライバ内部のエンジンセマフォ**
  (`ppa_priv.h` の `ppa_engine_t.sem`) をフレームの間保持して行う。
  PM ロックも同様に保持する。
- IDF のドライバは「セマフォ保持中 = 実行中のトランザクションがあり、その完了
  ISR がエンジンキューを捌く」前提。non-blocking クライアントがフレーム中に
  キューに積んだトランザクションはそのままでは誰も開始しないので、
  `end_frame` でキューを確認し、あればセマフォを保持したまま代わりに
  `dma2d_enqueue()` する (`ppa_transaction_done_cb()` と同じ手順)。
- ストリップバッファの入力側 cache sync は行わない (DMA しか書かないため)。
  出力バッファの M2C invalidate はフレームの前後に 1 回ずつ。
- ホスト版 (`ppa_srm_fast_host.c`) は `idf_compat` の PPA シムを同期的に呼ぶ。

### IDF バージョン依存

`jpeg_private.h` (engine 構造体・ヘッダパーサ・ディスクリプタテーブル)、
`ppa_priv.h` (PPA クライアント/エンジン構造体) と
`esp_private/dma2d.h` という **ESP-IDF の非公開ヘッダに依存**する。
構造体レイアウトが変わると黙って壊れるため、`jpeg_decode_enhanced.c` と
`ppa_srm_fast.c` に `ESP_IDF_VERSION` ガードがあり、検証済みは **v6.0.x〜v6.1.x**。別バージョンへ
移行する際は private 構造体のレイアウトを照合してから
`JPEG_DECODE_ENHANCED_SKIP_IDF_VERSION_CHECK` を定義する。

`esp_driver_jpeg` / `esp_driver_ppa` をコンポーネントごと差し替える (override)
方式は意図的に採っていない。

v5.4 → v6.0 で対応した非公開 API の変更点:

- 色フォーマットが `COLOR_TYPE_ID` から FOURCC (`esp_color_fourcc_t`) 方式へ。
  `color_hal_pixel_format_get_bit_depth()` → `color_hal_pixel_format_fourcc_get_bit_depth()`、
  `dma2d_desc_pixel_format_to_pbyte_value()` は fourcc を直接取る (`jpeg_dec_output_format_t` が既に fourcc 値)。
- HAL 分割: `dqt_func/sof_func/dht_func` と JPEG マーカー定数 (`JPEG_M_*`) は
  新 `esp_hal_jpeg` の公開ヘッダへ移動。`esp_private/dma2d.h` は `esp_driver_dma` へ。
- `dma2d_transfer_ability_t.data_burst_length` が enum から生 `uint32_t` (バイト数) へ。
- CSC レジスタのチャネル配列化: `dev->in_channel0` → `dev->in_channel[0]`。
- `codec_base->pm_lock` は `#if CONFIG_PM_ENABLE` ガード内に。

## シミュレータ (ホスト) バックエンド

同じ公開 API (`jpeg_ppa_pipeline.h` / `jpeg_decode_enhanced.h`) をホストでも提供し、
アプリを実機と同一コードでビルド・実行できる。Layer 2 (`jpeg_ppa_pipeline.c`) は
両ターゲット共有。Layer 1 だけホスト実装 (`jpeg_decode_enhanced_host.c`) に差し替わり、
2D-DMA ストリップ分割の代わりに `image_framework` の CPU JPEG デコーダで全画面を
1 ストリップとしてデコードする (PSRAM 帯域はホストに存在しない制約なので分割は不要)。

ホスト固有のセマンティクス:

- `strip_bufs` はストリップモードの有無の判定にだけ使い、書き込まない。
  常に 1 ストリップ (`strip_count == 1`) で、フレームごとに確保し直す作業バッファへ
  デコードする。
- `on_frame_start` / `on_strip_done` は `process()` 呼び出しスレッドから同期的に呼ばれる
  (ISR なし)。`on_strip_done` の戻り値は無視。
- `release_strip()` / `sync_strip_for_cpu()` は no-op。
- 出力は **RGB565 / RGB888 のみ** (パネルが RGB)。GRAY/YUV は `ESP_ERR_NOT_SUPPORTED`。
- transform (回転/スケール/ミラー/クロップ/配置) は `idf_compat` の PPA SW シムが処理し、
  実機と視覚的に等価な結果をプレビューする。RGB565 パッキング・`rgb_order` は
  `idf_compat` の `jpeg_decode.c` 規約 (R 上位詰め) に一致させてある。
