# アンチエイリアス設定説明書

XRizer の実験的アンチエイリアス（AA）の設定リファレンスです。対象は Linux（WiVRn／Monado）で XRizer 経由で動作する Vulkan ゲームです。既定では無効です。

## 設定ファイル

- 場所: `~/.config/xrizer/upscaler.json`（`XDG_CONFIG_HOME` が設定されている場合は `$XDG_CONFIG_HOME/xrizer/upscaler.json`）
- 環境変数 `XRIZER_UPSCALER_CONFIG` で任意のパスに変更できます
- 500 ms ごとに自動で再読み込みされます（ホットリロード）。ゲームの再起動は不要です
- ファイルが壊れている場合は、最後に正常だった設定が使われ続けます
- すべての項目は省略可能です。省略した項目には既定値が使われます。範囲外の数値は自動で範囲内に丸められます

## 設定項目

| 項目 | 型 | 既定値 | 範囲 | 説明 |
|---|---|---|---|---|
| `enabled` | bool | `false` | — | AA 全体の有効／無効 |
| `antialias` | string | `"fxaa"`※ | `off` / `cmaa2` / `smaa` / `fxaa` | AA 方式 |
| `weight_mode` | string | `"inverted"` | `center` / `inverted` / `whole` | 重み場のモード |
| `center` | [x, y] | `[0.5, 0.5]` | 各 0.0〜1.0 | 重み場の中心（眼テクスチャ UV）。両眼共通 |
| `center_offset` | number | `0.0` | -0.20〜0.20 | 眼ごとの x 補正。左眼は `-offset`、右眼は `+offset` |
| `radius` | number | `0.35` | 0.0〜1.0 | 重み場の内側境界 |
| `feather` | number | `0.10` | 0.0〜1.0 | `radius` の外側への遷移幅 |
| `strength` | number | `1.0` | 0.0〜1.0 | AA 結果と元画像の混合比 |
| `preserve_alpha` | bool | `true` | — | alpha < 0.999 のピクセルを変更しない |
| `fxaa_quality` | string | `"quality"` | `performance` / `balanced` / `quality` | FXAA の探索品質 |
| `smaa_threshold` | number | `0.1` | 0.05〜0.2 | SMAA のエッジ検出閾値 |
| `compare` | bool | `false` | — | `compare_interval_ms` ごとに AA の on/off を切り替える |
| `compare_interval_ms` | integer | `2000` | 500〜10000 | A/B 比較の切り替え周期 |
| `show_region` | bool | `false` | — | `radius` の等高線を表示（`center` / `inverted` のみ） |
| `auto_scale` | bool | `false` | — | ゲーム側の描画倍率の自動調整。AA とは別 |
| `min_scale` | number | `0.5` | 0.1〜4.0 | `auto_scale` の下限 |
| `max_scale` | number | `1.5` | 0.1〜4.0 | `auto_scale` の上限 |

`min_scale` が `max_scale` より大きい場合は入れ替えて解釈します。

※ B1b の暫定既定は `fxaa` です。B2 で `cmaa2` に変更します。B1b では `cmaa2` / `smaa` を選んでも FXAA にフォールバックします。

## 重み場

`d` は `center` からのアスペクト補正付き放射距離です。`w` は `strength` を掛ける前の AA 重みです。

| モード | 式 | 意味 |
|---|---|---|
| `center` | `w = 1 - smoothstep(radius, radius + feather, d)` | 中心で最大、外側で0 |
| `inverted` | `w = smoothstep(radius, radius + feather, d)` | 中心は0、周辺で最大 |
| `whole` | `w = 1.0` | 全面一様 |

- `feather = 0` のときは `radius` で hard step になります
- `whole` では `center` / `center_offset` / `radius` / `feather` を無視します
- `show_region` は `center` / `inverted` で `radius` の等高線を描き、`whole` では描きません

## 動作

- 出力解像度は常に入力と同じです。アップスケーリングは行いません
- `antialias = off` では AA パスを実行しません
- 方式が使えない場合は `cmaa2 -> fxaa`、`smaa -> fxaa`、`fxaa -> off` の順でフォールバックします
- `compare` は AA の効果だけを on/off します。画像サイズや他の設定は変わりません
- `preserve_alpha` は alpha < 0.999 のピクセルを元のまま保持します
- AA はゲームのプロジェクションレイヤーにだけ適用され、オーバーレイには適用されません

## 廃止キー

次のキーは v1 では読みません。設定アプリは未知キーとして保持し、新規に書き込みません。

- `preset`
- `output_scale`
- `model_path`
- ML モデル／ML プリセット関連キー
- `antialias_stage`

## 注意事項

- 実験的機能です。実機検証は RX 7900 XTX + VRChat／Proton を想定しています
- MSAA 提出は既存の resolve 経路を使います。AA はシングルサンプル時のみです
- `auto_scale` はゲーム側の `xrizer_scale` を調整する機能で、AA の解像度とは無関係です
