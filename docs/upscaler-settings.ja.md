# 空間アップスケーラ設定説明書

XRizer の実験的空間アップスケーラの設定リファレンスです。対象は Linux（WiVRn／Monado）で XRizer 経由で動作するゲームのみで、Vulkan 専用です。既定では無効です。

## 設定ファイル

- 場所: `~/.config/xrizer/upscaler.json`（`XDG_CONFIG_HOME` が設定されている場合は `$XDG_CONFIG_HOME/xrizer/upscaler.json`）
- 環境変数 `XRIZER_UPSCALER_CONFIG` で任意のパスに変更できます
- 500ms ごとに自動で再読み込みされます（ホットリロード）。ゲームの再起動は不要です
- ファイルが壊れている場合は、最後に正常だった設定が使われ続けます
- すべての項目は省略可能です。省略した項目には既定値が使われます。範囲外の数値は自動で範囲内に丸められます

## 設定項目

| 項目 | 型 | 既定値 | 範囲 | 説明 |
|---|---|---|---|---|
| `enabled` | bool | `false` | — | アップスケーラ全体の有効／無効 |
| `preset` | string | `"balanced"` | `fast`／`balanced`／`fsr`／`ml` | 処理方式（後述） |
| `center` | [x, y] | `[0.5, 0.5]` | 各 0.0〜1.0 | 強調の中心。歪み補正前の眼テクスチャ UV 空間での位置 |
| `radius` | number | `0.35` | 0.05〜2.0 | 強調の重みが最大になる半径 |
| `feather` | number | `0.10` | 0.0〜1.0 | `radius` の外側で重みが 0 へ落ちるまでの幅 |
| `strength` | number | `1.0` | 0.0〜3.0 | 強調（ML 残差とシャープニング）の強さ。基礎リサンプルには影響しません |
| `output_scale` | number | `1.0` | 0.5〜2.0 | 出力解像度の倍率。ランタイム推奨サイズに対して乗算されます |
| `preserve_alpha` | bool | `true` | — | 半透明ピクセル（alpha < 0.999）を強調せずそのまま保持します |
| `compare` | bool | `false` | — | 2秒ごとに強調の有効／無効を切り替える A/B 比較用。基礎リサンプルは常に有効です |
| `show_region` | bool | `false` | — | `radius` の等高線（重み最大の境界）を画面に描きます。最終パスが描くため、`compare` の off 区間では表示されません。調整用です |
| `model_path` | string／null | `null` | — | `ml` プリセットのモデル（XRU1 形式）のパス。`null` の場合は `~/.config/xrizer/upscaler_model.xru` |
| `auto_scale` | bool | `false` | — | フレーム時間を見て、ゲーム側のスーパーサンプル倍率を自動調整します |
| `min_scale` | number | `0.5` | 0.1〜4.0 | `auto_scale` の倍率の下限 |
| `max_scale` | number | `1.5` | 0.1〜4.0 | `auto_scale` の倍率の上限。`min_scale` より小さい場合は入れ替えて解釈されます |

## プリセット

| プリセット | 基礎リサンプル | 強調 | 備考 |
|---|---|---|---|
| `fast` | バイリニア | 重み付き簡易シャープ | 最も軽い |
| `balanced` | SGSR 1（全面） | 適応シャープニング + 後段 RCAS（重み付き） | 既定 |
| `fsr` | FSR 1 EASU | 重み付き RCAS | AA＋トーンマップ済みの入力が前提 |
| `ml` | FSR 1 EASU | 学習モデル（残差 CNN）の残差＋重み付き RCAS | モデルと DP4A が必要 |

`balanced` は SGSR 1 の出力を中間画像に書き、その後段で RCAS を適用する 2 パス構成です。`compare` の off 区間は RCAS が SGSR の出力をそのまま返します（基礎リサンプルは常に有効）。`show_region` の等高線は最終パス（RCAS）が描くため、`compare` の off 区間では表示されません（`fast`/`fsr` と同じ）。

フォールバック（いずれもゲームはそのまま動作します）:

- `ml` でモデルが無い・読み込めない場合は `fsr` に落ちます
- `ml` で DP4A（`shaderIntegerDotProduct`）が使えない場合は `fsr` に落ちます
- `balanced`（SGSR 1）は、テクスチャの bounds が反転している場合は `fast` に落ちます
- 強調処理そのものに失敗した場合は、アップスケールなしで元のフレームが提出されます

## 重み場（強調のかかり方）

強調はゾーン分けではなく、中心からの距離で連続的に決まります。

```
d = center からの放射距離（歪み補正前の UV 空間、アスペクト補正付き）
w = 1 - smoothstep(radius, radius + feather, d)
```

- 強調量は `w × strength` で、ML の残差・RCAS・適応シャープニングにだけ掛かります
- `balanced` では、SGSR の適応シャープニングと後段 RCAS の両方に `w × strength` が掛かります
- 基礎リサンプルは重みに関係なく全面に適用されます。重み 0 の場所も画像は完成しています
- 画面全体を強調したい場合は `radius` を大きくします（`2.0` で実質全面）。全体モードという別設定はありません
- ML の計算は `w` が 0 でない範囲の外接矩形に限定されます

## Vulkan レイヤー（`ml` プリセット用）

`ml` の DP4A 経路には、ゲーム（DXVK）が作る Vulkan デバイスへ `shaderIntegerDotProduct` 等を追加で有効化する implicit layer が必要です。`fast`／`balanced`／`fsr` には不要です。

```sh
# 導入（ユーザーディレクトリへ。Steam Linux Runtime ではこちらを推奨）
scripts/install-vulkan-layer.sh --user
# 削除
scripts/install-vulkan-layer.sh --remove

# 有効化（ゲームの起動オプション等に設定）
XRIZER_UPSCALER_VK_LAYER=1 %command%

# 無効化（有効化変数があっても必ず無効になります）
XRIZER_UPSCALER_VK_LAYER_DISABLE=1
```

- レイヤーは `vkCreateDevice` の横取りのみを行い、ドライバが対応しない機能は追加しません。失敗時は素通しし、ゲームの起動を妨げません
- 読み込まれたかは `VK_LOADER_DEBUG=layer` の出力で `VK_LAYER_XRIZER_upscaler` を確認できます

## 設定例

中心だけを SGSR 1 で強調する（既定に近い構成）:

```json
{
  "enabled": true,
  "preset": "balanced",
  "center": [0.5, 0.5],
  "radius": 0.35,
  "feather": 0.10,
  "strength": 1.0
}
```

全面を FSR 1 で処理する:

```json
{
  "enabled": true,
  "preset": "fsr",
  "radius": 2.0,
  "strength": 1.0
}
```

## 注意事項

- 実験的機能です。RX 7900 XTX＋VRChat／DXVK＋HMD での実機検証は未実施です
- `ml` のモデルは学習済みデータが別途必要です。同梱のモデルは動作確認用の小規模な構成です
- 性能の目安と計測方法は `docs/upscaler-benchmark.md` を参照してください
