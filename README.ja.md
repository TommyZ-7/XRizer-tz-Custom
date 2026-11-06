# xrizer - OpenVR ゲームを XR 化する

[English](README.md) | 日本語

xrizer は OpenVR を OpenXR 上に再実装したものです。SteamVR を起動せずに、任意の OpenXR ランタイム経由で OpenVR ゲームを実行できます。

xrizer はまだ発展途上です。動かない部分も多いので、バグ報告歓迎です。より枯れた解決策としては [OpenComposite](https://gitlab.com/znixian/OpenComposite) も検討してください（本リポジトリの一部のコードはこれを基にしています）。

本フォーク (TommyZ-7/xrizer) の独自機能については「[本フォークでの作業](#本フォークでの作業)」を参照してください。

# ダウンロードと使い方

[Latest nightly](https://nightly.link/Supreeeme/xrizer/workflows/ci/main/xrizer-nightly-release.zip) - 最新版！

[Latest release](https://github.com/Supreeeme/xrizer/releases/latest/download/xrizer-release.zip)

いずれかを選んでクリックし、zip をダウンロードして好きな場所に展開してください。

xrizer を使うには、OpenVR ゲームがランタイムを探す場所を変更する必要があります。方法は2つあります：

1. xrizer へのパスを `$XDG_CONFIG_HOME/openvr/openvrpaths.vrpath` に追加する。
2. ゲーム起動時に `VR_OVERRIDE` 環境変数を設定する：`VR_OVERRIDE=/path/to/xrizer <command>`（Steam の起動オプションでは `VR_OVERRIDE=/path/to/xrizer %command%`）
   - この方法でも、有効な `openvrpaths.vrpath` ファイルが既に存在している必要があります（xrizer 自体が含まれている必要はありません）。

## openvrpaths.vrpath

このファイルは OpenVR ゲームがランタイムの場所を特定するために読む JSON ファイルです。SteamVR をインストール済みで一度でも起動したことがあれば、「runtime」セクションに xrizer へのパスを追加するだけです。
そうでない場合、`$XDG_CONFIG_HOME/openvr/openvrpaths.vrpath` に置けるサンプルは以下です（パスは書き換えてください）：
```json
{
    "version": 1,
    "runtime": [
        "/path/to/xrizer"
    ]
}
```

## Steam Linux Runtime

Proton 6 以降を使う場合など、Steam Linux Runtime 経由でゲームを実行するとコンテナになるため、ファイルシステムのパスはそのままでは使えません。注意点：
- ホームディレクトリ以下のパスは常に利用可能
- `/usr` 以下のパスは `/run/host/usr` で利用可能
- 一部のパスは完全に利用不可

`PRESSURE_VESSEL_FILESYSTEMS_RW` 環境変数でパスをコンテナ内に共有できます。Monado をシステムワイドにインストールしている場合の Steam 起動オプション例：
```
XR_RUNTIME_JSON=/run/host/usr/share/openxr/1/openxr_monado.json VR_OVERRIDE=/path/to/xrizer PRESSURE_VESSEL_FILESYSTEMS_RW=$XDG_RUNTIME_DIR/monado_comp_ipc
```
コンテナの詳細は [Valve の Pressure Vessel ドキュメント](https://gitlab.steamos.cloud/steamrt/steam-runtime-tools/-/blob/main/pressure-vessel/wrap.1.md)を参照してください。

# FAQ

## どのゲームが動きますか？

ぜひ教えてください！目標は、標準的な（オーバーレイ／ユーティリティ／バックグラウンドでない）OpenVR アプリが SteamVR 上と同様に動作することです。もちろん 100% ではありません。動かないゲームがあれば issue を開いてください。順次対応します。

## なぜ OpenComposite を書き直すのですか？

OpenComposite は xrizer より数年長く存在するため、書き直しは小さな作業ではありません。一方で OpenComposite には十分なテスト基盤がなく、気づかないうちにリグレッションを混入させやすい上、アーキテクチャ的にシンプルなテストを書きにくいという問題があります。また当初は OpenXR を使う設計ではなかったため、その頃のレガシーな部分が残っており、理解を難しくしています。しばらくこれらと付き合った結果、書き直す方が生産的だと判断しました。

## なぜ Rust なのですか？

Rust が好きだからです。CMake は好きではありません。

# 本フォークでの作業

upstream ([Supreeeme/xrizer](https://github.com/Supreeeme/xrizer)) に対する本フォーク独自の変更です。Arch Linux (2026-09) でテストしています。

## 1. 動的解像度 (VRChat 向け)

upstream の xrizer は `IVRSettings::SetFloat("steamvr", "supersampleScale")` を無視します。本フォークではこれを実装し、`IVRSystem::GetRecommendedRenderTargetSize` をスケーリングすることで、VRChat (Unity OpenVR、OVRDR の動作確認済みリストでは Working) が再起動なしで RenderTexture を再確保できるようにします。`compositor` 側はスワップチェーンを再利用または再作成します（`src/compositor.rs` `is_usable_swapchain`）。

### 仕組み
* `src/settings.rs` — `steamvr/supersampleScale` の `SetFloat/GetFloat`（0.1–4.0 にクランプ）+ OVRDR 互換の `supersampleManualOverride` 用 `SetBool/SetInt32/GetBool/GetInt32`。
* `src/system.rs` — `GetRecommendedRenderTargetSize` が `recommended * scale` を返す。
* Proton（コンテナ）とホスト間のプロセス横断はファイルポーリング（`src/settings.rs` `maybe_reload_scale`、500 ms 毎）。`SetFloat` もファイルに永続化するため、外部ツールからの変更が伝播します。

### 操作方法

**1) 手動 — ファイルに echo（最もシンプル、コンテナ内でも常に動作）：**
```sh
echo 0.7 > ~/.config/xrizer_scale        # 70%（0.7 でも 70 でも可、4超はパーセント扱い）
echo 150 > ~/.config/xrizer_scale        # 150% -> 1.5
echo 1.0 > ~/.config/xrizer_scale        # リセット
# 500 ms 以内にポーリングされ、VRChat は次の GetRecommendedRenderTargetSize でリサイズ
# 検索順: $XRIZER_SCALE_FILE, $XDG_RUNTIME_DIR/xrizer_scale, ~/.config/xrizer_scale, ~/.local/share/xrizer_scale, /tmp/xrizer_scale
```

**2) 環境変数で上書き：**
```sh
XRIZER_SCALE_FILE=/tmp/my_scale VR_OVERRIDE=... %command%
echo 0.8 > /tmp/my_scale
```

**3) OpenVR API — OVRDR／自作ツール（自動でファイル連携）：**
```cpp
vr::VRSettings()->SetFloat(vr::k_pch_SteamVR_Section, vr::k_pch_SteamVR_SupersampleScale_Float, 0.7f);
// OVRDR の場合: https://github.com/Erimelowo/OpenVR-Dynamic-Resolution をビルドして実行（内部で SetFloat を呼ぶ）
vr::VRSystem()->GetRecommendedRenderTargetSize(&w,&h); // w/h がスケールされていることを確認
```
OVRDR の WorkingGames.md では VRChat は Working とされており、本フォークでは OVRDR が GPU/CPU フレームタイムに応じて再起動なしで VRChat の解像度を調整します。

**注意：**
* クランプ 0.1–4.0（UI の可読性のため下限 0.5 推奨）。拡大（現状より大きく）するとスワップチェーン再作成ログ + 約 30 ms のヒッチあり。縮小はスワップチェーン再利用。
* WiVRn のエンコード解像度は別物。VRChat 側と WiVRn 側のスケールを両方下げると最大の節約になります。
* デバッグ：`RUST_LOG=openvr_calls,system=debug cargo xbuild` 後に `grep supersampleScale ~/.local/state/xrizer/xrizer.txt`。

## 2. ディスプレイ輝度（ゲームレイヤーのみ）

`XR_KHR_composition_layer_color_scale_bias` でゲームのプロジェクションレイヤーのみを暗くします（`src/compositor.rs` `end_frame`）。オーバーレイ（WayVR 等）は明るいままです。`1.0` = 通常、`0.5` = 半分、`0.0` = 真っ黒。0.0–1.0 にクランプ。拡張に対応したランタイムが必要（WiVRn v26.9／拡張 ON の Monado ビルド）。非対応ランタイムでは無視されます。

```sh
echo 0.5 > ~/.config/xrizer_brightness   # 半分（0.5 でも 50 でも可、1超はパーセント扱い）
echo 1.0 > ~/.config/xrizer_brightness   # リセット
# xrizer_scale と同じ形式: $XRIZER_BRIGHTNESS_FILE, $XDG_RUNTIME_DIR/xrizer_brightness, ~/.config/xrizer_brightness, ~/.local/share/xrizer_brightness, /tmp/xrizer_brightness
```

```cpp
vr::VRSettings()->SetFloat("xrizer", "brightness", 0.5f); // ファイルにも永続化
```

## 3. 実験的アンチエイリアス（Vulkan）

既定は無効。設定は `~/.config/xrizer/upscaler.json`（`XRIZER_UPSCALER_CONFIG` で上書き）に置き、500 ms ごとにホットリロードします。壊れた JSON の場合は直前の設定を維持します。

このフォークは AA 専用です。提出画像のサイズは変更せず、出力解像度は常に入力と同じです。B1a でアップスケーラのプリセット、EASU、SGSR、ML モデル、Vulkan 機能レイヤーを撤去しました。B1b で等倍 FXAA を実装しました。B2 で CMAA2、B3 で SMAA 1x を追加します。それまでは `cmaa2` / `smaa` は FXAA にフォールバックします。B1b の暫定既定は `fxaa` で、B2 で `cmaa2` に変更します。

重み場は3モードです:

- `center`: 中心で最大、`radius + feather` の外側で0（旧 `field_weight`）。
- `inverted`: `radius` の内側で0、周辺で最大（既定）。
- `whole`: 全面一様。`center` / `center_offset` / `radius` / `feather` は無視。

`center_offset` は左右眼で逆方向に中心を補正します。0.6.4 の検証で左右の円の位置がズレて見えたための項目です。

`~/.config/xrizer/upscaler.json` の例:

```json
{
  "enabled": true,
  "antialias": "fxaa",
  "weight_mode": "inverted",
  "center": [0.5, 0.5],
  "center_offset": 0.0,
  "radius": 0.35,
  "feather": 0.10,
  "strength": 1.0,
  "preserve_alpha": true,
  "fxaa_quality": "quality",
  "smaa_threshold": 0.1,
  "compare": false,
  "compare_interval_ms": 2000,
  "show_region": false,
  "auto_scale": false,
  "min_scale": 0.5,
  "max_scale": 1.5
}
```

旧キー `preset` / `output_scale` / `model_path` / ML 関連キーは無視され、未知キーとして保持されます。`auto_scale` / `min_scale` / `max_scale` はゲーム側の描画倍率で、AA パスとは別です。

診断はカウンタのみ（画像内容なし）: フレーム数、AA フレーム数、FXAA/CMAA2/SMAA、フォールバック数、最後の理由。300 フレームごとに `$XDG_STATE_HOME/xrizer/xrizer.txt` へ要約を記録します。MSAA は既存の resolve 経路です。

### 学習用キャプチャ（opt-in）

Task 14 の学習データ用に、ゲームが提出した眼テクスチャ（アップスケール前・ランタイム歪み補正前）を無劣化 8bit PNG で保存できます。明示的に有効化しない限り読み戻しや追加コピーは発生しません。

```sh
XRIZER_CAPTURE=1 \
XRIZER_CAPTURE_DIR="$HOME/xrizer-captures/VRChat/scene-a" \
XRIZER_CAPTURE_MODE=interval \
XRIZER_CAPTURE_INTERVAL_MS=500 \
XRIZER_CAPTURE_MAX_PAIRS=300 \
  %command%
```

コントローラで撮影する場合は `XRIZER_CAPTURE_MODE=shutter` を指定し、**両手のグリップ＋トリガーを同時に 250ms 保持**します。1 回の押下で 1 ペア、1 秒のロックアウト付きです（`XRIZER_CAPTURE_HOLD_MS` / `XRIZER_CAPTURE_LOCKOUT_MS`）。入力は XRizer の OpenXR アクション経由で読むだけで消費しないため、ゲーム側にもそのまま伝わります。シャッター検出はゲームのアクション同期（VRChat や多くの action-manifest タイトル）に相乗りします。interval モードは入力に依存しません。

`frame_000001_left.png` / `frame_000001_right.png` と `metadata.json`（解像度、フォーマット、色空間、bounds、`render_scale`、プリセット、眼ごとの情報）を保存します。`XRIZER_CAPTURE_MAX_PAIRS`（既定 300 ペア）で自動停止し、VR 内ホットキーはありません。`XRIZER_CAPTURE=0` で無効化できます。shutter モードで撮影に成功すると、フレーム番号と保存先を含む freedesktop 通知（D-Bus）をベストエフォートで送ります。通知サーバがない場合でも撮影・ゲームは継続します。詳細は `tools/upscaler_dataset.md`。PNG エンコードは提出スレッド外で行い、提出側は別コマンドバッファの記録と fence ポーリングのみです。MSAA はスキップし、HDR/10bit は 8bit にクリップされます。

## 4. パフォーマンス改善

* 両目のコピーを 1 回の submit にまとめ、フレーム毎のオーバーヘッドを削減。
* 輝度レイヤー処理のフレーム毎ヒープ確保を排除。
* リリースプロファイル最適化（`Cargo.toml`: lto、codegen-units=1、strip）。シェーダーは `glslc -O` でコンパイル。

# ビルド
```
# dev ビルド
cargo xbuild

# release ビルド
cargo xbuild --release
```

ビルド後、出力ディレクトリをそのままランタイムディレクトリとして使えます。dev ビルドなら `<xrizer リポジトリ>/target/debug`、release ビルドなら `<xrizer リポジトリ>/target/release` です。

## Arch Linux — ビルドとインストール

### 1. 依存パッケージ
```sh
sudo pacman -S base-devel rust clang llvm pkgconf glslang vulkan-headers openxr
# rust >=1.85 が必要（Cargo.toml の rust-version 参照）
# glslang はシェーダーコンパイル用の glslc を提供
# 代替: rustup + toolchain 1.85
```

`glslc --version` がなければ `glslang` が未インストールです。

### 2. ビルド

```sh
git clone https://github.com/TommyZ-7/xrizer.git
cd xrizer

cargo xbuild              # debug -> target/debug/libxrizer.so
cargo xbuild --release    # release -> target/release/libxrizer.so
# target/(debug|release)/bin/linux64/vrclient.so -> libxrizer.so としても出力
```

動的解像度に追加の Cargo feature は不要です。WiVRn/Monado 向けにはデフォルトの `monado` で十分です。

### 3. インストール／使い方

upstream と同じで、どちらか一方を選んでください：

**a) VR_OVERRIDE（Steam/Proton 向け推奨、システム設定不要）：**
Steam > VRChat > プロパティ > 起動オプション：
```
VR_OVERRIDE=/home/$USER/xrizer/target/release %command%
```
既存の `$XDG_CONFIG_HOME/openvr/openvrpaths.vrpath` が必要です（空の JSON で可、下記参照）。

**b) openvrpaths.vrpath：**
`$XDG_CONFIG_HOME/openvr/openvrpaths.vrpath` にランタイムパスを追加：
```json
{
  "version": 1,
  "runtime": ["/home/USER/xrizer/target/release"]
}
```

**Arch で WiVRn (Monado) をシステムワイドに使う場合：**
```
XR_RUNTIME_JSON=/run/host/usr/share/openxr/1/openxr_wivrn.json \
VR_OVERRIDE=/home/$USER/xrizer/target/release \
PRESSURE_VESSEL_FILESYSTEMS_RW=$HOME/.config \
%command%
```
`PRESSURE_VESSEL_FILESYSTEMS_RW` でスケールファイルを Proton コンテナに共有します（ファイル経由の動的解像度に必須）。

ランタイム確認：
```sh
RUST_LOG=info VR_OVERRIDE=/home/$USER/xrizer/target/release vrchat 2>&1 | grep xrizer
# ログ: $XDG_STATE_HOME/xrizer/xrizer.txt
```

xrizer が報告するバージョンは `git describe` 由来です（デフォルトの `git-version` feature 経由）。git チェックアウトがないビルド（配布パッケージやソース tarball からのビルド等）では `--no-default-features` で無効化し（必要なデフォルト feature、例 `--features monado` は再有効化）、代わりにビルド時に `XRIZER_VERSION` を設定してください。`XRIZER_VERSION` が設定されている場合は常に `git describe` より優先されます。

# 貢献

すべての貢献を歓迎します。
- バグ報告の際はログを添付してください。ログは `$XDG_STATE_HOME/xrizer/xrizer.txt`（未設定なら `$HOME/.local/state/xrizer/xrizer.txt`）にあります。
- プルリクエストを送る際は、可能／有用であればテストの記述を検討してください。OpenVR は API 面が広くゲームは気まぐれなので、テストで守ることが将来の意図しない破壊を防ぎます。

# 環境変数
_RUST_LOG_ - xrizer のログ調整に使います。[env_logger のドキュメント](https://docs.rs/env_logger/latest/env_logger/#enabling-logging)を参照してください。便利な非標準ログターゲット：
- `openvr_calls` - 呼び出された OpenVR 関数名をログ
- `tracked_property` - 要求されたトラッキングデバイスプロパティの名前とデバイスインデックスをログ

_XRIZER_CUSTOM_BINDINGS_DIR_ - xrizer がコントローラーバインディングファイルを探すディレクトリを指定できます。バインディングの形式は実際にはどこにも文書化されていませんが、既存ファイルの改変は容易で、xrizer がパースするのでソースも読めます。

_XRIZER_TRACKER_SERIALS_ - 汎用トラッカーとして使うデバイスシリアル番号のセミコロン（`;`）区切りリスト。コントローラーを FBT トラッカーとして割り当てるのに使えます。

_XRIZER_SCALE_FILE_ - 動的解像度のスケールファイルのパスを上書きします（本フォークのみ）。未設定時は `$XDG_RUNTIME_DIR/xrizer_scale` → `~/.config/xrizer_scale` → `~/.local/share/xrizer_scale` → `/tmp/xrizer_scale` の順に探します。

_XRIZER_BRIGHTNESS_FILE_ - 輝度のスケールファイルのパスを上書きします（本フォークのみ）。探索順はスケールファイルと同様です。

# 関連項目

- [OpenComposite](https://gitlab.com/znixian/OpenOVR) - 元祖の OpenVR/OpenXR 実装。xrizer よりずっと枯れています。本リポジトリの一部のコードは OpenComposite を基に書き直されました。
- [OpenVR](https://github.com/ValveSoftware/openvr)
- [Monado](https://gitlab.freedesktop.org/monado/monado) - オープンソースの OpenXR ランタイム
- [WiVRn](https://github.com/WiVRn/WiVRn) - Monado ベースのスタンドアロンヘッドセット向け OpenXR ストリーミングランタイム
