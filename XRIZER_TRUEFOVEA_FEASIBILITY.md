# XRizer における TrueFovea 相当機能の実装可能性調査

- 調査日: 2026-11-04
- 対象: `/home/tommyz7/work-saeki/xrizer`（TommyZ-7/xrizer フォーク、`main` @ `30b31d7`）
- 参照: `/home/tommyz7/work-saeki/rea-analysis/TrueFovea_1_ra7k/ANALYSIS.md`（以下 `ANALYSIS.md`）および同 `ANALYSIS_evidence/`
- 制約: **静的調査のみ**。コード変更・ビルド・実機実行は行っていない。工数はコード構造からの概算であり、実測ではない。
- 本レポートは法的助言ではない。ライセンス判断は必要に応じて専門家に確認すること。

---

## 0. 結論（要約）

**TrueFovea の主要なユーザー体験（低解像度で描画した VRChat の映像を、視線または固定領域を中心に GPU で超解像・輪郭強調する）は、XRizer に実装可能である。ただし、TrueFovea のアーキテクチャをそのまま移植することはできない。**

TrueFovea は SteamVR のユーザーモードドライバとして `vrserver.exe` 内にロードされ、MinHook で `IVRServerDriverHost_006::TrackedDeviceAdded` をフックして実 HMD の `IVRDriverDirectModeComponent_008/009` をラップし、`SubmitLayer` / `Present` の眼テクスチャを加工する（`ANALYSIS.md` §0, §3.3, §3.4）。一方 XRizer は SteamVR ではなく、ゲームプロセスにロードされる `vrclient.so` として OpenVR を OpenXR に変換する（`README.md:5`）。したがって対応関係は次のように置き換わる。

| TrueFovea（SteamVR ドライバ） | XRizer（OpenVR → OpenXR クライアント） |
|---|---|
| `CreateSwapTextureSet` / 影テクスチャセット | OpenXR スワップチェーン / 中間テクスチャ（`FrameController::init_swapchain`） |
| `SubmitLayer` | `IVRCompositor::Submit` → `FrameController::submit_impl` |
| `Present` | `PostPresentHandoff` → `end_frame` → `xrEndFrame` |
| DirectMode 008/009 のラップ | 不要。ゲームの `Submit` を直接横取りできる |
| MinHook / `vrserver.exe` へのロード | 不要。プロセス内で完結 |
| アプリ ↔ ドライバの共有メモリ | 不要。同一プロセスの設定状態で足りる |

**判定サマリ**

| 機能要素 | 判定 | 備考 |
|---|---|---|
| 空間補正（超解像・輪郭強調） | ◎ 実現可能 | `Submit` のコピー経路をシェーダパスに置換する。本命 |
| 固定中央・全体モード | ◎ 実現可能 | 画面 UV の中心/全体。アイトラッカー不要 |
| 視線追従（OpenXR eye gaze） | ○ 条件付き | `XR_EXT_eye_gaze_interaction` と HMD/ランタイム/許可に依存 |
| 視線追従（VRChat OSC / VRCFT） | ○ 条件付き | UDP リスナを追加。ポート競合と Proton ネットワークに注意 |
| 時間補正 | △ 困難 | モーションベクタ/深度がなく、ブロックマッチングの実装が必要。工数大 |
| 小型学習ネットワーク | △ 条件付き | 推論は容易。TrueFovea の重みは流用不可。FSR 1 などの代替が現実的 |
| 設定アプリ | ○ 一部可能 | 設定ファイル + 外部 GUI は可能。Win32 トレイ/自動起動/ドライバ登録は非移植 |
| OSC 連携 | ○ 条件付き | 可能。システム全体で 1 ポートを共有するならデーモン化が必要 |
| ドライバ ↔ アプリ共有メモリ | − 不要 | プロセス境界がない。外部ツール連携時のみ代替 IPC を検討 |
| 半透明オーバーレイ保護 | ◎ 実現可能 | XRizer ではオーバーレイが別レイヤーなので構造的に保護される |
| 解像度スケーリング / 自動倍率 | ◎ / ○ | 動的解像度はフォークに既存。自動制御は追加実装 |
| SteamVR ドライバ / DirectMode / MinHook | × 対象外 | XRizer は SteamVR を置き換える。ドライバとして振る舞わない |
| ミラー取得 + 投影オーバーレイ（1.0.3） | × 標準 OpenXR では不可 | 最終合成画像の標準 readback がない |
| システム全体（SteamVR/他アプリ含む） | × XRizer の範囲外 | XRizer 経由のゲームのみ。全体適用は OpenXR レイヤー/ランタイム改造 |
| Windows / D3D11 | × 現行コードでは不可 | 現行バックエンドは Vulkan / OpenGL のみ |

**工数の目安（1 名、XRizer と Vulkan に習熟した開発者を想定）**

| 範囲 | 目安 |
|---|---|
| MVP: Vulkan 空間補正 + 固定中央/全体 + 設定ファイル + 半透明保護 + A/B + テスト | **15〜25 人日（3〜5 週間）** |
| 推奨 v1: 上記 + OpenXR/OSC 視線 + 簡易キャリブレーション + 自動倍率 + 診断 | **25〜45 人日（5〜9 週間）** |
| フル: 上記 + OpenGL 対応 + オーバーレイ補正 + 外部 GUI + 時間補正 + 学習ネットワーク | **65〜125 人日（3〜6 人月）** |

Vulkan の描画パイプラインや XRizer のコードベースに不慣れな場合、MVP は 1.5〜2 倍見ておくのが安全である。実機テスト用の HMD/GPU/WiVRn 環境も工数に含めること。

**最大のギャップ**

1. **最終合成画像を取得できない。** XRizer が見るのはゲームが `Submit` した眼テクスチャであり、OpenXR ランタイムが合成した最終画像ではない。TrueFovea の「最終映像全体を補正する」体験とは、合成前後という違いが残る。
2. **XRizer 経由のゲームにしか効かない。** SteamVR 全体や他の OpenVR/OpenXR アプリを巻き込むには、OpenXR API レイヤーまたは Monado/WiVRn のコンポジタ側に実装する必要がある。
3. **Windows/D3D11 は現行 XRizer の対象外。** TrueFovea の HLSL/D3D11 資産をそのまま使うことはできず、Vulkan/GLSL への移植が必要。
4. **TrueFovea のシェーダ・重み・定数は流用できない。** `TERMS.txt` は逆アセンブル/逆コンパイルと独自部分の製品への組み込みを禁止している。仕様レベルの独立実装か、FSR 1（MIT）などの公開アルゴリズムを使うべきである。
5. **時間補正は技術的難度が高い。** モーションベクタも深度もないため、TrueFovea と同様のブロックマッチングを自前で書く必要があり、ゴースティング/遅延の検証コストも大きい。

---

## 1. 前提と対象範囲

### 1.1 対象とする XRizer の状態

本フォークは Linux 向けで、Arch Linux + WiVRn/Monado での利用が README に記載されている（`README.ja.md:65-67`）。現行のグラフィックスバックエンドは **Vulkan と OpenGL のみ**で、D3D11/D3D12 は実装されていない（`src/graphics_backends.rs:62-70`, `:120-141`）。Proton 経由の D3D11 ゲームは DXVK で Vulkan に変換されるため、Vulkan パスが主要経路になる。

フォーク独自機能として、以下が既に実装されている。

- **動的解像度**: `IVRSettings::SetFloat("steamvr","supersampleScale")` を実装し、`GetRecommendedRenderTargetSize` をスケーリングする（`src/settings.rs:9`, `:32`, `:193`; `src/system.rs:198-229`; `README.ja.md:69-106`）。
- **ゲームレイヤー限定の輝度調整**: `XR_KHR_composition_layer_color_scale_bias` をプロジェクションレイヤーに付与する（`src/compositor.rs:1383-1415`; `README.ja.md:108-120`）。
- **両眼コピーの一括 submit** などの性能改善（`src/compositor.rs:1287-1292`; `git log` `0c0793a`）。

本レポートはこの状態を前提に、**Linux / Vulkan（必要なら OpenGL）/ Monado or WiVRn** を主対象とする。Windows/SteamVR を主対象にする場合は、後述の代替アーキテクチャ（SteamVR ドライバまたは OpenXR API レイヤー）を検討すべきである。

### 1.2 TrueFovea の前提

TrueFovea 本番 1.0.2 は Windows x64 / SteamVR / D3D11 向けで、VRChat の VR モードのみを対象とする（`ANALYSIS.md` §0, §2.1）。構成は以下。

- `TrueFovea.exe`: OpenVR Background アプリ。設定、OSC、視線、解像度スケーリング、ドライバ登録、診断、トレイ常駐を担当（`ANALYSIS.md` §3.1, §3.7-§3.12）。
- `driver_truefovea.dll`: SteamVR の `IServerTrackedDeviceProvider_004`。`vrserver.exe` 内で `IVRServerDriverHost_006::TrackedDeviceAdded` を MinHook でフックし、実 HMD の `ITrackedDeviceServerDriver` をラップする。DirectMode 008/009 の `CreateSwapTextureSet` / `SubmitLayer` / `Present` を横取りし、D3D11 シェーダで補正する（`ANALYSIS.md` §3.3-§3.5）。
- アプリとドライバは名前付き共有メモリ `Local\TrueFovea.v23.data`（0x348 bytes）とビルド確認イベントで連携する（`ANALYSIS.md` §3.6）。

### 1.3 本レポートで扱わないこと

- 実装そのもの、ビルド、実機での画質/性能検証。
- TrueFovea バイナリの追加解析。既存の `ANALYSIS.md` の範囲を超える逆コンパイルは行っていない。
- VRChat / SteamVR / HMD ベンダの規約の法的評価。

---

## 2. TrueFovea の機能分解

`ANALYSIS.md` の観測事実を、実装判断に必要な単位に分解する。

| ID | 要素 | TrueFovea の実装（観測/推論） | 根拠 |
|---|---|---|---|
| F1 | 空間補正 | DirectMode の眼テクスチャに D3D11 ピクセルシェーダを適用。4 近傍/方向性/S カーブ/3 次補間などのアルゴリズムと、学習ネットワーク（`psLearned`）を持つ。 | `ANALYSIS.md` §3.5.1, §3.5.2 |
| F2 | 時間補正 | 4 スロットの観測バッファ、ブロックマッチング、Capture→Score→Reduce→Initialize→Prepare→Forward→Backward のコンピュートシェーダ。エンジンのモーションベクタは使わない。失敗時は空間補正へフォールバック。 | `ANALYSIS.md` §3.5.3 |
| F3 | 視線 / 固定領域 | 円形・矩形・投影平面の 3 種類の領域判定。`screenToPlane[3]`、`regionDistance`、`regionWeight`、`feather`。固定中央・全体モードはアイトラッカー不要。 | `ANALYSIS.md` §3.5.1, §3.8 |
| F4 | 学習ネットワーク | 5×5 輝度パッチ（25）+ サブピクセル位相 2 + スケール 1 = 28 入力。4 隠れ×2 層（品質プリセットの一部は 8 隠れ×2 層）。出力は残差。重みは cbuffer に定数として埋め込み。 | `ANALYSIS.md` §3.5.2 |
| F5 | 設定アプリ | Win32 UI、トレイ常駐、Windows 自動起動、言語切替、A/B 比較ラベル、視線キャリブレーション。OpenVR Background アプリ + `IVROverlay`。 | `ANALYSIS.md` §3.1, §3.10, §3.12 |
| F6 | OSC | UDP。既定受信 `127.0.0.1:19090`、転送 OFF、LAN OFF。VRChat の `/avatar/parameters/...` と VRCFT の `/tracking/eye/...` を解析。 | `ANALYSIS.md` §3.9 |
| F7 | アプリ ↔ ドライバ IPC | 名前付き共有メモリ + ミューテックス + ビルド確認イベント。設定/状態/GPU 情報を交換。 | `ANALYSIS.md` §3.6 |
| F8 | 半透明オーバーレイ保護 | `alpha < 0.999` のピクセルは補正せず保持（`preserveTransparency`）。 | `ANALYSIS.md` §3.5.1 |
| F9 | 解像度スケーリング / 自動倍率 | VRChat の `steam.app.438100` の `resolutionScale` を int % で読み書き。倍率 1〜3×、自動は最大 5×。GPU/CPU/FPS を見て段階調整。 | `ANALYSIS.md` §3.7 |
| F10 | SteamVR ドライバ / DirectMode | `HmdDriverFactory`、`IServerTrackedDeviceProvider_004`、MinHook 1 箇所、DirectMode 008/009 のラップ。 | `ANALYSIS.md` §3.3, §3.4 |
| F11 | ミラー + 投影オーバーレイ（1.0.3） | `IVRCompositor::GetMirrorTextureD3D11` で左右眼ミラーを取得し、`IVROverlay::SetOverlayTransformProjection` + `SetOverlayTexture` で投影表示。ゲームの解像度は変更しない。 | `ANALYSIS.md` §5 |
| F12 | 診断 / プライバシー | `data/support/*.json`。画像・視線座標・生 OSC・アバター名・シリアル・IP・絶対パスを含めない。自動送信なし。 | `ANALYSIS.md` §3.11 |
| F13 | HMD 固有経路 | Bigscreen Beyond 2e / PS VR2 向けの検出と投影確認ツール。ドライバなしでミラー + オーバーレイ。 | `ANALYSIS.md` §4, §5 |

---

## 3. XRizer の現状アーキテクチャ

### 3.1 全体像

```text
VRChat (Proton/DXVK)                  ← OpenVR アプリ
  │  IVRCompositor::Submit(eye, VkImage, bounds, flags)
  ▼
vrclient.so = XRizer                   ← ゲームプロセス内
  ├─ Compositor::Submit                （src/compositor.rs:738）
  ├─ FrameController::submit_impl      （src/compositor.rs:1225）
  ├─ copy_texture_to_swapchain         （src/graphics_backends/vulkan.rs:202）
  │    └─ VkImage → XrSwapchain（array layer = eye）
  ├─ OverlayMan::get_layers            （src/overlay.rs:164）
  └─ FrameController::end_frame        （src/compositor.rs:1306）
       └─ xrEndFrame(projection layer + overlay layers)
  ▼
OpenXR ランタイム（Monado / WiVRn）
  ├─ 合成・レンズ歪み・色収差
  ├─ WiVRn の場合はエンコード/ストリーム
  ▼
HMD
```

XRizer は **コンポジタを持たない**。OpenXR ランタイムが最終合成を担当し、XRizer は「ゲームの眼テクスチャを OpenXR スワップチェーンに変換して渡す」役割を持つ。これが TrueFovea との決定的な違いである。

### 3.2 映像経路の接点

| 処理 | 場所 | 補足 |
|---|---|---|
| ゲームからの Submit | `src/compositor.rs:738-823` | bounds 検証、focus 確認、frame controller 取得 |
| スワップチェーン作成/再利用 | `src/compositor.rs:1057-1165` | `is_usable_swapchain` でサイズ/フォーマットを判定 |
| 眼テクスチャのコピー | `src/graphics_backends/vulkan.rs:202-353` | `cmd_copy_image` / `cmd_blit_image` / `cmd_resolve_image` |
| 両眼の一括 submit | `src/graphics_backends/vulkan.rs:355-363` | `flush_copies`。フォークの性能改善 |
| プロジェクションレイヤー構築 | `src/compositor.rs:1317-1381` | `xr::CompositionLayerProjectionView` |
| オーバーレイレイヤー | `src/overlay.rs:164-333` | quad / cylinder / equirect2。別レイヤー |
| `xrEndFrame` | `src/compositor.rs:1427-1429` | projection + overlays |
| ミラー取得 | `src/compositor.rs:412-431` | すべて `todo!()` / `IncompatibleVersion` |
| `SubmitWithArrayIndex` | `src/compositor.rs:714-723` | `todo!()`。配列テクスチャ未対応 |
| 視線 API | `src/system.rs:478-493` | `GetEyeTrackedFoveationCenter` は未実装スタブ |

### 3.3 既存の拡張ポイント

- **`GraphicsBackend` トレイト**（`src/graphics_backends.rs:10-60`）: `copy_texture_to_swapchain` と `copy_overlay_to_swapchain` が抽象化されている。ここに補正パスを差し込むのが最も自然。
- **Vulkan の既存オーバーレイパイプライン**（`src/graphics_backends/vulkan.rs:728-1001`）: ゲームテクスチャを入力に、スワップチェーンへ描画するグラフィックスパイプラインが既にある。プロジェクション補正用に流用できる。
- **設定のファイルポーリング**（`src/settings.rs:16-68`, `:86-122`）: 500 ms ごとにファイルを読み、アトミック変数へ反映するパターンが既にある。多数のパラメータは JSON 設定ファイルに拡張できる。
- **動的解像度**（`src/system.rs:198-229`）: ゲームの描画解像度を下げる仕組みが既にある。補正の出力サイズをランタイム推奨サイズに固定すれば、低解像度 → 高解像度の超解像パイプラインが成立する。
- **オーバーレイの別レイヤー性**（`src/overlay.rs:220-235`）: オーバーレイは `BLEND_TEXTURE_SOURCE_ALPHA | UNPREMULTIPLIED_ALPHA` の別レイヤー。ゲームレイヤーだけ補正すれば、オーバーレイは構造的に保護される。

### 3.4 現状の制約

- **D3D11/D3D12 バックエンドがない**（`src/graphics_backends.rs:120-141`）。Windows の OpenVR ゲームが直接 D3D11 テクスチャを Submit する構成は現行コードでは扱えない。
- **最終合成画像の readback がない**（`src/compositor.rs:412-431`）。標準 OpenXR にはミラー/readback API がない。
- **`IVRApplications` がほぼ未実装**（`src/applications.rs:15-180`）。VRChat のシーンアプリ検出やプロセス ID 取得はできない。XRizer 自身がゲームプロセス内にいるため、補正対象の判定には不要だが、TrueFovea のように `IVRApplications` で VRChat を検出する外部設定アプリを作る際は制約になる。
- **`GetEyeTrackedFoveationCenter` が未実装**（`src/system.rs:478-493`）。OpenVR 2.15.6 のヘッダには API が存在する（`openvr/headers/openvr-2.15.6.h:2564-2569`）。
- **`SubmitWithArrayIndex` が未実装**（`src/compositor.rs:714-723`）。配列テクスチャを使うゲームでは既に問題になる可能性がある。
- **OpenGL バックエンドはシェーダパスを持たない**（`src/graphics_backends/gl.rs:225-299` はコピー/ブリットのみ）。

---

## 4. 機能別フィージビリティ

### 4.1 F1: 空間補正（超解像・輪郭強調） — ◎ 実現可能

#### 4.1.1 基本方針

`copy_texture_to_swapchain` の「コピー」を「ゲームテクスチャをサンプリングして、補正済みの眼画像をスワップチェーンに描画する」パスに置き換える。

```text
現在:
  game VkImage --cmd_copy_image/blit--> XrSwapchain layer(eye)

提案:
  game VkImage --fragment shader (upscale + sharpen + fovea weight + alpha preserve)--> XrSwapchain layer(eye)
```

Vulkan では既存の `PipelineData`（オーバーレイ用）と同様のグラフィックスパイプラインを作り、入力画像をサンプラで読み、スワップチェーンのカラーアタッチメントへ描画する。両眼分のコマンドバッファは既に `2 * image_count` 確保されている（`src/graphics_backends/vulkan.rs:144-154`）ため、`flush_copies` の一括 submit にもそのまま乗る。

実装上の注意として、現在の `GraphicsBackend::copy_texture_to_swapchain` は `&self`（`src/graphics_backends.rs:35`）だが、パイプラインキャッシュを遅延作成するには `&mut self` に変更するか、内部可変性（`Mutex`/`OnceCell`）を使う必要がある。`FrameController` 側は `&mut self` を持ち、`copy_overlay_to_swapchain` は既に `&mut self` なので、`&mut self` への変更が素直である（テスト用の `FakeGraphicsData` も合わせて更新）。

#### 4.1.2 出力サイズの分離（重要）

現行のスワップチェーンは **ゲームテクスチャのサイズ**で作られる（`src/graphics_backends/vulkan.rs:176-200`）。これだとゲームが 0.7× で描画した場合、XRizer のスワップチェーンも 0.7× になり、ランタイムが最終的に拡大する。補正の効果を最大化するには、**出力スワップチェーンをランタイム推奨サイズ（`recommended_view_size`）に固定**し、シェーダで入力 → 出力のスケーリングを行う。

- ランタイム推奨サイズは `OpenXrData::recommended_view_size` にキャッシュされている（`src/openxr_data.rs:46-48`, `src/system.rs:198-215`）。
- `FrameController::submit_impl` で `new_info.width/height` を出力サイズに上書きする、または `swapchain_info_for_texture` に出力サイズを渡す。
- `SubmittedEye.extent` を出力サイズにし、`xr::SwapchainSubImage::image_rect` を全領域にする（`src/compositor.rs:1357-1363`）。
- `is_usable_swapchain` は出力サイズの一致を要求する（入力サイズが変わっても出力は不変なので、ゲームの動的解像度変更でスワップチェーンを作り直さずに済む）。これはフォークの動的解像度と非常に相性が良い。
- 出力サイズは `recommended * output_scale`（既定 1.0）とし、ユーザーが 0.8〜1.2 程度で調整できるようにする。

この変更により、ゲームは `supersampleScale` で低解像度に描画し、XRizer がランタイム推奨サイズへ超解像してから渡す構成になる。TrueFovea の「VRChat の解像度を下げてドライバで補正」に最も近い。

#### 4.1.3 アルゴリズム候補

| プリセット | アルゴリズム | コスト | 備考 |
|---|---|---|---|
| Fast | バイリニア + RCAS/CAS 系の適応シャープ | 低 | 1 パス。まずはこれで動作確認 |
| Balanced | Catmull-Rom / Lanczos2 + 適応シャープ | 中 | 1 パス。輪郭とリンギングのバランス |
| Quality | FSR 1 EASU + RCAS（2 パス） | 中〜高 | MIT。EASU は方向性適応、RCAS はシャープ |
| Learned | 5×5 パッチ + 小型 MLP（推論のみ） | 低（推論） | 重みは自前調達。TrueFovea の重みは流用不可 |

FSR 1 は MIT ライセンスで、Vulkan/GLSL の実装が公開されている。EASU は `textureGather` を使うため Vulkan では問題なく動く見込みだが、GL バックエンドでは別途 GLSL を用意する必要がある。まずは Fast/Balanced を自前の GLSL で実装し、Quality で FSR 1 を追加するのが現実的。

FSR 1 の EASU は sRGB/ガンマ空間の入力を前提とする。XRizer のスワップチェーンは sRGB のため、sRGB テクスチャを通常にサンプリングするとリニア値になる。FSR 1 を適用する場合は、UNORM ビューでサンプリングするか、リニア → sRGB 変換を挟んでから EASU に渡し、出力側で再度エンコードするなど、色空間の扱いを明確にする必要がある。Fast/Balanced の自前シェーダも、リニアでシャープ化するかガンマ空間でシャープ化するかで見た目が変わるため、A/B で決める。

#### 4.1.4 半透明・フォーマット・境界の扱い

- **半透明保護**: フラグメントシェーダで `src.a < 0.999` なら補正せず元ピクセルを出力する。TrueFovea の `preserveTransparency` と同じ考え方（`ANALYSIS.md` §3.5.1）。ただし XRizer ではオーバーレイが別レイヤーなので、ゲーム内の半透明 UI のみが対象になる。
- **bounds / 上下反転**: `VRTextureBounds_t` の `uMin/uMax/vMin/vMax` を UV に反映する。既存の `texture_extent_from_bounds`（`src/graphics_backends/vulkan.rs:1033-1055`）と `flip_vertically`（`src/compositor.rs:1274`, `:1353-1355`）の扱いを踏襲する。
- **配列テクスチャ**: `EVRSubmitFlags::VulkanTextureWithArrayData` の `m_unArrayIndex` を入力レイヤーとして使う（既存コピー経路は `src/graphics_backends/vulkan.rs:211-217`, `:262-264` で対応済み）。ただし `SubmitWithArrayIndex` 自体が未実装なので、その経路は別途実装が必要。
- **MSAA**: `texture.m_nSampleCount > 1` の場合、フラグメントシェーダからマルチサンプル画像を直接サンプリングできない。MVP では「MSAA 時は補正を無効化して既存コピーにフォールバック」し、必要なら一度 resolve 用中間テクスチャに解決してから補正する。
- **フォーマット**: 入力は `R8G8B8A8_SRGB` / `B8G8R8A8_SRGB` / `BC3_SRGB` など（`src/graphics_backends/vulkan.rs:1005-1031`）。出力スワップチェーンは `check_format` でランタイム対応フォーマットに補正される（`src/openxr_data.rs:612-637`）。シェーダは sRGB サンプリング → リニア計算 → sRGB 出力を基本とする。`EColorSpace::Linear` は現状 `todo!()` なので、この経路は既存の制約として残る。
- **色空間**: 出力が UNORM にフォールバックした場合、シェーダ側でガンマ変換が必要になる可能性がある。テストマトリクスに入れる。

#### 4.1.5 変更範囲と工数

| ファイル | 変更内容 | 目安 |
|---|---|---|
| `src/graphics_backends.rs` | `copy_texture_to_swapchain` に出力 extent / 補正パラメータを追加 | 20〜40 行 |
| `src/compositor.rs` | 出力サイズ決定、`SubmittedEye.extent`、設定の受け渡し、A/B | 100〜200 行 |
| `src/graphics_backends/vulkan.rs` | 補正用パイプライン、デスクリプタ、レンダーパス、画像ビュー、フォールバック | 400〜800 行 |
| `src/graphics_backends/gl.rs` | FBO + シェーダパス（Phase 3） | 300〜500 行 |
| `shaders/src/` | フルスクリーン vertex + 補正 fragment、FSR 1 | 150〜400 行 |
| `src/foveation.rs`（新規） | 設定、領域重み、視線→UV、状態管理 | 300〜500 行 |
| テスト | 設定、領域重み、出力サイズ、フォーマット分岐 | 200〜400 行 |

**MVP の目安: 15〜25 人日。** Vulkan の既存オーバーレイパイプラインを流用できるため、ゼロからコンポジタを書くよりは軽い。ただし、フォーマット・bounds・MSAA・配列・sRGB の分岐を詰めるのに時間がかかる。

**最初の PoC（2〜3 人日）** としては、出力サイズを変えず、既存の `PipelineData` を流用して入力サイズのまま 1 パスのシャープをかけるのがよい。これで VRChat の見た目と GPU コストの当たりを付け、その後で出力サイズ分離（4.1.2）に進む。PoC の段階で「思ったより効果が見えない」「リンギングが強い」なら、アルゴリズムと出力サイズの設計を早めに修正できる。

---

### 4.2 F3: 視線 / 固定領域 — ◎（固定）/ ○（視線）

ここでいう foveation は、描画時の foveated rendering（視線周辺の描画解像度を変える）ではなく、**後段の補正強度・領域を視線に追従させる post-process foveation** である。TrueFovea も最終映像に対する後段処理であり、同じ位置づけになる。

#### 4.2.1 固定中央・全体

- 固定中央: UV 中心 `(0.5, 0.5)`、半径 `radius`、フェザー `feather`。円形/矩形を設定で選ぶ。
- 全体: 領域重みを 1.0 に固定。画面全体を均一に補正。
- アイトラッカー不要。MVP はこれで十分。

#### 4.2.2 OpenXR eye gaze

OpenXR の `XR_EXT_eye_gaze_interaction` は、インタラクションプロファイル `/interaction_profiles/ext/eye_gaze_interaction` と、ポーズアクション `/user/eyes_ext/input/gaze_ext/pose` で視線を取得する。XRizer の OpenXR インスタンスは拡張を `ExtensionSet` で管理しており、`other` に拡張名を追加できる（`src/openxr_data.rs:128-155`。Monado の `XR_MNDX_XDEV_SPACE` で同じパターンを使用）。`openxr` crate 0.22 に型付きの eye gaze ヘルパーがなくても、`Instance::create_action::<xr::Posef>` と `string_to_path` で実装できる見込み。

実装手順:

1. `enumerate_extensions()` の `other` に `XR_EXT_eye_gaze_interaction` があるか確認し、あれば `exts.other` に追加してインスタンスを作成。拡張が列挙されても実際に使えるとは限らないため、`XrSystemEyeGazeInteractionPropertiesEXT::supportsEyeGazeInteraction` も確認する（`openxr` crate が未対応なら raw FFI）。`XrEyeGazeSampleTimeEXT` でサンプル時刻を取得できる。
2. 内部アクションセット `xrizer-gaze-set` とポーズアクション `gaze` を作成。サブアクションパスは `/user/eyes_ext`。
3. `xrSuggestInteractionProfileBindings` で `/interaction_profiles/ext/eye_gaze_interaction` にバインド。
4. セッション開始時にアクションセットを attach し、毎フレーム `xrSyncActions` する。XRizer の既存の内部アクション（info/haptic）と同じ扱い（`src/input/action_manifest.rs:114-176`, `src/input/legacy.rs:91-101`）。仕様上 `xrAttachSessionActionSets` はセッション開始前が原則なので、既存コードの attach タイミングを確認し、必要ならセッション再作成時にまとめて attach する。
5. `xrLocateSpace` で gaze ポーズを取得。視線方向を HMD の view space に変換し、設定距離（既定 1.5 m）の平面と交差させて各眼の UV を計算する。
6. 取得できない/古い/許可なしの場合は固定中央へフォールバック。`GetEyeTrackedFoveationCenter` / `...ForProjection`（`src/system.rs:478-493`）も同じデータで実装できる。ゲーム側にも視線を提供できるようになる。

注意点:

- ランタイム/HMD/ユーザー許可に依存する。WiVRn は Pico 等の eye tracking に対応しているが、全 HMD で使えるわけではない。Monado は PSVR2 等で eye gaze を実装している。
- 視線のサンプルレートと表示タイミングが一致しない場合がある。TrueFovea もキャリブレーションと保持で対処している。XRizer では「最新サンプル + 短い保持 + 平滑化」で実用上は足りる見込み。
- 視線データはプライバシー機微。既定でログに出さない。

#### 4.2.3 OSC / VRCFT

VRChat の OSC 出力（`/avatar/parameters/EyeX`, `EyeY`, `EyeLeftX`, `EyeLeftY`, `EyeRightX`, `EyeRightY` など）と、VRChat 標準の eye tracking（`/tracking/eye/CenterPitchYaw`, `CenterVec`, `LeftRightVec` など）を UDP で受信する。TrueFovea は 19090 番で受信し、VRCFT の `/tracking/eye/...` も解析している（`ANALYSIS.md` §3.8, §3.9）。

実装:

- `std::net::UdpSocket` を非ブロッキングで専用スレッドに置き、`rosc` などの OSC パーサでメッセージ/バンドルを解析。
- 既定は `127.0.0.1:19090`、LAN は既定 OFF、転送は既定 OFF。ポートは設定可能。
- VRChat の OSC 出力ポートと一致させる必要がある。VRChat 側の設定はユーザー責任。
- 10 秒タイムアウト（VRChat 標準）より短い 0.2〜1 秒で「古い視線」と判定し、固定中央へフォールバックする。TrueFovea は送信元プロセスの生存監視で静止保持するが、XRizer ではまずタイムアウト方式でよい。
- Proton コンテナ内の XRizer がホストの OSC を直接受け取れるかは環境依存。確実にするなら、ホスト側で OSC を受ける小さなデーモンを作り、Unix ドメインソケット/共有メモリで XRizer に渡す。これは Phase 2 のオプション。
- 複数の VR ゲームが同時に走る場合、1 つの UDP ポートを奪い合う。通常は 1 つだけなので問題になりにくいが、デーモン化するなら解消できる。
- リスナスレッドはライブラリのアンロード時に確実に停止する（`Drop` または `AtomicBool` + join）。不正な OSC パケットで panic しないよう、サイズ/型を検証する。

#### 4.2.4 キャリブレーション

- MVP: 設定ファイルで `gaze_offset_x/y`, `gaze_gain_x/y`, `plane_distance` を調整。固定中央なら不要。
- フル: HMD 内に 5 点/9 点のマーカーを出し、注視サンプルからアフィン変換を推定。TrueFovea の `data/gaze-calibration-*.txt` に相当。XRizer では内部オーバーレイ（`CompositionLayerQuad` + 生成テクスチャ）で点を描く必要があり、UI 実装コストがかかる。Phase 3。

**工数**: 固定中央/全体は 1〜2 人日。OpenXR eye gaze は 5〜10 人日（アクションセット、視線→UV、フォールバック、テスト）。OSC/VRCFT は 3〜6 人日。簡易キャリブレーションは 3〜5 人日。フル 5/9 点 UI は 5〜10 人日。

---

### 4.3 F2: 時間補正 — △ 困難だが技術的には可能

TrueFovea の時間補正は、4 スロットの観測バッファとブロックマッチングで前フレームからの変位を推定し、Capture→Score→Reduce→Initialize→Prepare→Forward→Backward のコンピュートシェーダを実行する（`ANALYSIS.md` §3.5.3）。エンジンのモーションベクタは使わない。

XRizer で実装する場合の課題:

- **入力が最終画像ではない**: ゲームの眼テクスチャのみ。深度やモーションベクタはない。TrueFovea と同じくブロックマッチングが必要。
- **履歴管理**: 眼ごと・解像度ごとに履歴テクスチャを保持し、ゲームの解像度変更やセッション再起動で破棄する必要がある。
- **複数パス**: Capture/Score/Reduce/Initialize/Prepare/Forward/Backward を Vulkan コンピュートで実装すると、パイプライン/デスクリプタ/バリアのコード量が大きい。既存の Vulkan バックエンドはグラフィックス/コピー中心で、コンピュートパイプラインの基盤がない。
- **ゴースティング/遅延**: UI、パーティクル、高速移動で破綻しやすい。マスクとフォールバックが必要。
- **効果の検証**: 実機で「空間補正のみ」と「時間補正あり」を比較し、遅延・ゴースト・ちらつきを評価する必要がある。

**現実的な代替**:

1. **空間補正のみ**で v1 を出す（推奨）。
2. **軽量時間フィルタ**: ヘッドポーズの差分で前フレームを再投影し、近傍クランプでゴーストを抑える 1 パス方式。オブジェクト運動には弱いが、実装コストは低い。
3. **ランタイムの時間補正/フレーム補間**（`XR_EXT_frame_synthesis` など）に任せる。XRizer の外側の機能であり、Monado/WiVRn の対応に依存する。

**工数**: 本格的なブロックマッチング時間補正は 20〜40 人日。軽量時間フィルタは 5〜10 人日。研究・検証込みならさらに +50%。

---

### 4.4 F4: 小型学習ネットワーク — △ 条件付き

TrueFovea の学習ネットワークは 28 入力 → 4/8 隠れ × 2 層 → 1 出力の小さな MLP で、推論はピクセルシェーダ内で完結する（`ANALYSIS.md` §3.5.2）。**推論自体は XRizer でも容易**である。問題は重みの入手とライセンス。

- TrueFovea の重みは cbuffer に定数として埋め込まれており、`ANALYSIS.md` でも全重みは復元されていない。仮に復元しても、`TERMS.txt` は逆アセンブル/逆コンパイルと独自部分の製品への組み込みを禁止している。**コピーしてはいけない。**
- 選択肢:
  1. **FSR 1 EASU + RCAS** を Quality プリセットに使う。MIT。学習ではないが、方向性適応アップスケール + シャープで、TrueFovea の「画質」に近い体験になる。
  2. **自前の小型 MLP** を訓練する。5×5 輝度 + サブピクセル位相 + スケールを入力にし、高解像度画像との差を学習。推論シェーダは 1 パスで軽い。訓練データ（VRChat の高解像度キャプチャ）と品質評価が必要。
  3. **単純な適応フィルタ**（方向性バイラテラル/ CAS）で妥協する。
- 自前 MLP の場合、モデル構造は TrueFovea と似ていてもよいが、重み・定数・シェーダコードは独立に作る。訓練データの利用規約（VRChat の撮影/配布）にも注意。

**工数**: FSR 1 統合は 3〜5 人日（Vulkan）。自前 MLP は訓練パイプライン込みで 10〜20 人日 + 品質評価。フルパリティの「学習ネットワーク」を名乗るなら、データ収集・評価を含めて 3〜6 週間を見るべき。

---

### 4.5 F5/F6: 設定アプリと OSC — ○ 一部可能

#### 4.5.1 設定

XRizer は共有ライブラリであり、TrueFovea のような Win32 常駐アプリは持たない。代替:

- **設定ファイル + ホットリロード**: `$XRIZER_FOVEATION_CONFIG` → `$XDG_CONFIG_HOME/xrizer/foveation.json` → `~/.config/xrizer/foveation.json` → `$XDG_RUNTIME_DIR/xrizer_foveation.json` → `/tmp/xrizer_foveation.json` の順で探し、500 ms ごとに再読込。既存の `maybe_reload_scale` と同じパターン（`src/settings.rs:16-68`）。Proton では `PRESSURE_VESSEL_FILESYSTEMS_RW=$HOME/.config` で共有できる（`README.md:125-130`）。外部 GUI は一時ファイル + rename で原子的に書き、XRizer は読み取り/パース失敗時に前回の設定を保持する。
- **OpenVR Settings API**: `IVRSettings::SetFloat/SetBool/SetString` の `xrizer` セクションに `foveation_enabled`, `foveation_strength`, `foveation_radius`, `foveation_mode` などを追加し、ファイルにも永続化する。既存の `brightness` と同じ方式（`src/settings.rs:193-218`）。
- **外部 GUI**: Rust + egui/eframe などで別プロセス GUI を作り、JSON を読み書きする。スライダー、プリセット、OSC 設定、A/B、キャリブレーションを提供。XRizer 本体に GUI を埋め込む必要はない。
- **A/B 比較**: 設定 `compare` で 2 秒ごとに補正 ON/OFF を切り替える。ラベルを出したい場合は内部オーバーレイに小さなテクスチャを描く（Phase 3）。TrueFovea の「A/B ラベル」に近い体験。ただし A 側は「元の高解像度描画」ではなく「低解像度描画を通常アップスケールしたもの」であり、TrueFovea の README と同じ注意点がある。
- **補正範囲の可視化**: シェーダで緑のリング/枠を描く。`show_region` 設定。低コスト。
- **キャリブレーション**: 4.2.4 の通り。フル UI は Phase 3。
- **トレイ常駐/自動起動/言語切替**: Linux では desktop file / systemd user service / GUI で代替。コア機能ではない。

#### 4.5.2 OSC

4.2.3 の通り。設定アプリが OSC を受けるのではなく、XRizer 本体またはホスト側デーモンが受ける。TrueFovea のように「アプリが OSC を受けて共有メモリでドライバに渡す」必要はない。

#### 4.5.3 共有メモリ（F7）

**不要**。TrueFovea の共有メモリは、`vrserver.exe` 内のドライバと別プロセスのアプリを繋ぐためのもの（`ANALYSIS.md` §3.6）。XRizer は同一プロセス内で設定を共有できる。外部 GUI/デーモンと連携する場合だけ、Unix ドメインソケットまたは小さな共有メモリを検討する。ビルド確認イベント/DirectMode のハンドル管理も不要。

**工数**: 設定ファイル + ホットリロード 2〜4 人日。外部 GUI 5〜10 人日。A/B + 範囲可視化 1〜2 人日。キャリブレーション UI 5〜10 人日。OSC 3〜6 人日。

---

### 4.6 F8: 半透明オーバーレイ保護 — ◎ 実現可能

XRizer では、ゲームのプロジェクションレイヤーとオーバーレイ（`IVROverlay`）が別の OpenXR レイヤーとして送られる（`src/compositor.rs:1417-1425`, `src/overlay.rs:220-235`）。したがって、**ゲームレイヤーだけを補正すれば、オーバーレイは構造的に補正されない**。これは TrueFovea の「最終画像の alpha < 0.999 を保持する」より強く、確実な保護になる。

- ゲーム内の半透明 UI は、フラグメントシェーダの alpha チェックで保護する。
- オーバーレイも補正したい場合（TrueFovea の `overlays=1` に相当）は、`copy_overlay_to_swapchain`（`src/overlay.rs:505-609`, `src/graphics_backends/vulkan.rs:365-565`）に同じシェーダを適用する。ただし overlay は quad/cylinder/equirect で、テクスチャサイズも小さい。テキストのリンギングを避けるため、強さを別設定にする。
- 外部オーバーレイアプリ（XSOverlay 等）は XRizer の主対象ではない（`README.md:53`）。WayVR などの OpenXR オーバーレイはランタイム側の別レイヤーであり、XRizer からは補正できない。保護はされるが、強化もされない。

**工数**: 保護のみなら 0.5〜1 人日（シェーダの alpha 分岐）。オーバーレイ補正は 2〜4 人日。

---

### 4.7 F9: 解像度スケーリング / 自動倍率 — ◎ / ○

- **手動スケーリング**: フォークに既存（`src/settings.rs:9-68`, `src/system.rs:198-229`）。TrueFovea の「倍率 1〜3×」は、XRizer では `supersampleScale = 1/倍率` に相当する。たとえば 1.5× 相当なら `0.667`。既存の `xrizer_scale` ファイルで制御できる。
- **TrueFovea の `resolutionScale` との違い**: TrueFovea は VRChat の `steam.app.438100` セクションの `resolutionScale`（int %）を読み書きする（`ANALYSIS.md` §3.7.2）。XRizer の `IVRSettings` は現状 `steamvr/supersampleScale` と `xrizer/brightness` 程度しか実装しておらず、`steam.app.*` の per-app 設定は扱わない（`src/settings.rs:137-335`）。同等の per-app 制御が必要なら、`IVRSettings` のセクション対応を追加するか、`supersampleScale` を外部ツールから制御する。
- **出力サイズ**: 4.1.2 の通り、補正出力をランタイム推奨サイズに固定する。これにより「ゲームは低解像度、XRizer が高解像度に補正」が成立する。
- **自動倍率**: TrueFovea は GPU 負荷/FPS を見て段階的に倍率を上げる（`ANALYSIS.md` §3.7.3）。XRizer では以下で近似する。
  - フレーム間隔（`WaitGetPoses`/`xrWaitFrame` の周期）から FPS を測る。
  - 補正パスの GPU 時間を Vulkan timestamp query で測る。
  - 目標フレーム時間に対する余裕を見て `SUPERSAMPLE_SCALE` を 1 段ずつ変更し、ヒステリシスを入れる。
  - 既存の `GetFrameTiming` は GPU 時間が固定値（`src/compositor.rs:613-643`）なので、実測するなら timestamp query の追加が必要。ゲーム全体の GPU 時間は取れないため、TrueFovea の「VRChat の実測 GPU 時間」と完全には一致しない。
- **注意**: 解像度変更はゲーム側の RenderTexture 再作成と XRizer 側のスワップチェーン再作成を伴う（フォークの `is_usable_swapchain`）。出力スワップチェーンを固定すれば、ゲーム側の変更で XRizer 側の出力を作り直す必要がなくなる。
- **WiVRn との関係**: ゲームを 0.7× で描いて XRizer が 1.0× に戻す場合、WiVRn のエンコード入力は 1.0× のままである。ゲームの GPU 負荷は下がるが、エンコード負荷/帯域は変わらない。ネットワークやエンコーダがボトルネックの場合、見た目は良くなってもフレームレート改善は限定的になる。逆に、WiVRn 側のストリーム解像度も下げるなら、XRizer の出力サイズも下げる設定が必要になる。

**工数**: 出力サイズ分離は 4.1 に含む。自動倍率は 4〜7 人日（timestamp query、制御、ヒステリシス、テスト）。

---

### 4.8 F10: SteamVR ドライバ / DirectMode / MinHook — × 対象外

TrueFovea の中核は SteamVR ドライバである（`ANALYSIS.md` §0, §3.3, §3.4）。XRizer には以下のものが存在しない。

- `vrserver.exe` へのロード、`IServerTrackedDeviceProvider_004`、`HmdDriverFactory`
- 実 HMD の `ITrackedDeviceServerDriver` / `IVRDriverDirectModeComponent_008/009`
- `SubmitLayer` / `Present`、影テクスチャセット
- MinHook による `TrackedDeviceAdded` フック
- `openvrpaths.vrpath` の `external_drivers` 登録、ビルド確認イベント

したがって、TrueFovea のドライバコードを XRizer に移植することはできない。**XRizer では `IVRCompositor::Submit` が DirectMode の `SubmitLayer` に相当する横取り点**であり、MinHook なしで同じ目的を達成できる。これは移植ではなく、アーキテクチャの置き換えである。

逆に、XRizer では実現できないこと:

- SteamVR 全体の最終映像を補正する（XRizer は SteamVR を使わない）。
- SteamVR のオーバーレイアプリ（XSOverlay 等）をまとめて補正する。
- HMD ドライバ固有のタイミング/歪み補正の直前に処理する。
- DirectMode の depth/共有テクスチャなど、ドライバ側の情報を使う。

---

### 4.9 F11: ミラー + 投影オーバーレイ（1.0.3） — × 標準 OpenXR では不可

TrueFovea 1.0.3 は `IVRCompositor::GetMirrorTextureD3D11` で左右眼のミラー画像を取得し、`IVROverlay::SetOverlayTransformProjection` + `SetOverlayTexture` で投影表示する（`ANALYSIS.md` §5.1-§5.3）。XRizer の `GetMirrorTextureD3D11` / `GetMirrorTextureGL` は未実装（`src/compositor.rs:412-431`）。

- OpenXR 標準には、ランタイムが合成した最終画像をアプリへ返す mirror/readback API がない。したがって、TrueFovea 1.0.3 と同じ「最終合成画像を取得してオーバーレイで重ねる」方式は、Monado/WiVRn の標準機能だけでは実現できない。
- ランタイム固有の拡張や内部 API があれば可能性はあるが、移植性はない。
- XRizer で近いことをするなら、ゲームの眼テクスチャを補正してプロジェクションレイヤーとして出す（4.1）のが正しい。最終合成画像に対する投影確認ツールは、ランタイム側（Monado/WiVRn のコンポジタ）に実装する必要がある。
- `XR_FB_composition_layer_settings` のようなランタイム側の sharpening/super-sampling 拡張も存在するが、Monado/WiVRn での対応は限定的で、標準的な代替にはならない。

---

### 4.10 F12: 診断 / プライバシー — ◎ 実現可能

- XRizer は既に `$XDG_STATE_HOME/xrizer/xrizer.txt` にログを出す（`src/lib.rs:128-153`）。補正の有効/無効、モード、視線ソース、入力/出力サイズ、GPU 時間、フォールバック理由、フレームカウンタを追加する。
- TrueFovea の診断 JSON に相当するものを出すなら、`foveation-status.json` を 1 秒ごとなどに更新する。画像・視線座標・生 OSC・アバター名・シリアル・IP は含めない。
- 自動送信なし。OSC は既定 localhost、LAN は既定 OFF。視線データは既定でログしない。
- GPU メモリ監視（TrueFovea の `pdh.dll`）は Linux では `VK_EXT_memory_budget` などで代替可能。必須ではない。

**工数**: 2〜3 人日。

---

### 4.11 F13: HMD 固有経路（Beyond 2e / PS VR2） — × 対象外

TrueFovea の Beyond/PSVR2 投影確認ツールは、SteamVR のミラー画像 + 投影オーバーレイ + HMD 検出に依存する（`ANALYSIS.md` §4, §5）。XRizer は HMD ドライバを持たず、OpenXR ランタイムが HMD を抽象化するため、HMD 固有の投影経路を実装する意味がない。HMD 固有の最適化が必要なら、Monado/WiVRn のドライバ側で行うべきである。

---

## 5. 推奨アーキテクチャと実装方針

### 5.1 推奨: XRizer の Submit パス内で補正する

```text
VRChat
  │ Submit(eye, game_texture)
  ▼
XRizer
  ├─ 出力 extent = runtime recommended size × output_scale
  ├─ 補正パラメータ = 設定/視線/固定中心
  ├─ フルスクリーンシェーダ:
  │    src_uv = bounds_map(dst_uv)
  │    c = sample(game_texture, src_uv)
  │    if (c.a < 0.999) out = c;            // 半透明保護
  │    else out = upscale_sharpen(c, region_weight(dst_uv, center))
  ├─ 両眼分をまとめて submit
  └─ xrEndFrame(projection + overlays)
  ▼
OpenXR runtime
```

この方式の利点:

- SteamVR ドライバ、MinHook、`vrserver.exe`、共有メモリが不要。
- ゲームの眼テクスチャを直接扱えるため、低解像度 → 高解像度の超解像が自然に書ける。
- オーバーレイが別レイヤーなので、半透明保護が構造的に成立する。
- フォークの動的解像度と組み合わせて、ゲームの描画負荷を下げつつ見た目を補正できる。
- 既存の Vulkan オーバーレイパイプラインとコマンドバッファ管理を流用できる。

欠点:

- 最終合成画像ではなく、ゲームレイヤーのみ。
- XRizer 経由のゲームのみ。
- Vulkan/OpenGL のみ。D3D11/Windows は別途。
- ランタイムの歪み/色収差/リプロジェクションの前に補正するため、最終画質はランタイム依存。

### 5.2 パイプライン詳細（Vulkan）

1. **設定**: `FoveationConfig` を JSON から読み、`Arc`/`RwLock` で保持。毎フレームのロックを避けるため、フレーム開始時にコピー。
2. **出力サイズ**: `recommended_view_size` を取得し、`output_scale` を掛けて `dst_extent` を決める。`FrameController::submit_impl` で `new_info.width/height` を上書き。
3. **スワップチェーン**: `is_usable_swapchain` を出力サイズ一致で判定。入力サイズが変わっても再作成しない。
4. **パイプラインキャッシュ**: `RealSessionData` に `ProjectionPipeline` を追加。キーは入力フォーマット、出力フォーマット、サンプル数、アルゴリズム。既存の `PipelineData::new`（`src/graphics_backends/vulkan.rs:755-1001`）を参考にする。
5. **レンダーパス**: 入力画像を `TRANSFER_SRC_OPTIMAL` から `SHADER_READ_ONLY_OPTIMAL` に遷移し、通常の `COMBINED_IMAGE_SAMPLER` で読む（既存オーバーレイの input attachment 方式は、ゲーム画像が `VK_IMAGE_USAGE_INPUT_ATTACHMENT_BIT` を持たない場合があるため、デスクリプタサンプラ方式を推奨）。出力スワップチェーンは `COLOR_ATTACHMENT_OPTIMAL`。描画後に元のレイアウトへ戻す。framebuffer と viewport は **出力 extent** で作り、シェーダ内で入力 UV をスケーリングする。
6. **画像ビュー**: スワップチェーンの array layer（eye）ごとに `VkImageView` を作る。既存の `PipelineData` は layer 0 のみなので、左右 2 つ必要。
7. **コマンド**: `copy_texture_to_swapchain` の中で、既存の `cmd_copy_image` の代わりに `cmd_begin_render_pass` → `cmd_draw(3 or 4)` → `cmd_end_render_pass` を記録。両眼分は `flush_copies` で一括 submit。
8. **フォールバック**: MSAA、未対応フォーマット、シェーダコンパイル失敗、出力サイズ 0、queue が graphics 非対応、視線無効時は既存コピーに戻す。ログに理由を出す。
9. **A/B**: `compare` が有効なら 2 秒ごとに `enabled` を切り替える。シェーダの uniform で分岐。
10. **診断**: 補正回数、フォールバック回数、GPU 時間、入力/出力サイズをカウンタで保持。

設定ファイルのイメージ（案）:

```json
{
  "enabled": true,
  "mode": "fixed-center",
  "preset": "balanced",
  "strength": 1.0,
  "radius": 0.35,
  "feather": 0.10,
  "output_scale": 1.0,
  "preserve_alpha": true,
  "overlays": false,
  "compare": false,
  "gaze": {
    "source": "auto",
    "plane_distance": 1.5,
    "timeout_ms": 500,
    "offset_x": 0.0,
    "offset_y": 0.0
  },
  "osc": {
    "listen": "127.0.0.1:19090",
    "lan": false,
    "forward": false
  }
}
```

`mode` は `fixed-center` / `full` / `gaze`、`preset` は `fast` / `balanced` / `quality` / `learned` を想定する。`auto` は OpenXR eye gaze が有効ならそれを使い、なければ OSC、最後に固定中央へフォールバックする。

### 5.3 変更範囲（概算）

| ファイル | 変更 | 目安 |
|---|---|---|
| `src/foveation.rs`（新規） | 設定、領域重み、視線→UV、状態、A/B、診断 | 300〜500 行 |
| `src/settings.rs` | JSON 設定のホットリロード、OpenVR Settings キー、既存 scale との連携 | 100〜200 行 |
| `src/compositor.rs` | 出力サイズ、`SubmittedEye.extent`、補正パラメータ、A/B、診断 | 100〜200 行 |
| `src/graphics_backends.rs` | trait の引数追加、`FoveationParams` | 30〜60 行 |
| `src/graphics_backends/vulkan.rs` | 補正パイプライン、画像ビュー、レンダーパス、フォールバック | 400〜800 行 |
| `src/graphics_backends/gl.rs` | FBO + シェーダ（Phase 3） | 300〜500 行 |
| `shaders/src/foveation.vert` / `.frag` | フルスクリーン + 補正 | 150〜300 行 |
| `shaders/src/lib.rs` | シェーダ追加 | 数行 |
| `src/system.rs` | `GetEyeTrackedFoveationCenter` 実装 | 50〜100 行 |
| `src/openxr_data.rs` | eye gaze 拡張の有効化、推奨サイズの公開 | 50〜100 行 |
| `src/input.rs` / `input/*` | 内部 gaze action set の attach/sync/locate | 100〜200 行 |
| `src/osc.rs`（新規） | UDP/OSC リスナ、パーサ、タイムアウト | 200〜400 行 |
| テスト | 設定、領域、視線、出力サイズ、OSC | 300〜600 行 |

**MVP（Vulkan 空間補正 + 固定/全体 + 設定 + A/B + 半透明保護 + テスト）: 約 1,500〜2,500 行 + シェーダ。**
**視線/OSC/自動倍率まで含むと +800〜1,500 行。**
**時間補正/学習ネットワークは別途 2,000〜4,000 行規模。**

### 5.4 実装フェーズ

| フェーズ | 内容 | 成果物 | 目安 |
|---|---|---|---|
| 0 | 設計、シェーダプロトタイプ、フォーマット/サイズの調査 | 技術メモ、見た目比較 | 3〜5 人日 |
| 1 | Vulkan 空間補正、固定中央/全体、出力サイズ分離、設定ファイル、A/B、半透明保護、基本テスト | MVP | 15〜25 人日 |
| 2 | OpenXR eye gaze、OSC/VRCFT、簡易キャリブレーション、自動倍率、診断 | 推奨 v1 | 10〜20 人日 |
| 3 | OpenGL 対応、オーバーレイ補正、外部 GUI、A/B ラベル、範囲可視化 | 配布候補 | 10〜20 人日 |
| 4 | 時間補正（ブロックマッチング or 軽量再投影） | 実験的機能 | 20〜40 人日 |
| 5 | 学習ネットワーク（自前訓練 or FSR 1 で代替） | 品質プリセット | 10〜20 人日 |

**推奨 v1（Phase 1+2）で 25〜45 人日（5〜9 週間）。** テスト、調整、ドキュメントを含めると +30〜50% を見込む。

### 5.5 テスト計画

- **ユニットテスト**:
  - 設定 JSON のパース/デフォルト/クランプ。
  - 領域重み（円/矩形/全体/フェザー）。
  - 視線→UV 変換（既知のポーズ/投影行列）。
  - 出力サイズ決定と `is_usable_swapchain` の再利用条件。
  - OSC メッセージ/バンドルのパース、タイムアウト。
  - alpha 保護の分岐（CPU リファレンス）。
- **fakexr テスト**: 既存の `FakeGraphicsData` を使い、`Submit` の出力 extent、スワップチェーン再利用、A/B の状態遷移を確認。シェーダ出力そのものは GPU が必要なので対象外。
- **手動/実機**:
  - VRChat + WiVRn/Monado で、固定中央/全体/視線/OSC の各モード。
  - フォーマット: RGBA8/BGRA8 sRGB、BC3、可能なら HDR/linear（現状 `todo!()`）。
  - シナリオ: 静止画、テキスト、細い線、パーティクル、高速移動、UI、半透明、解像度変更、セッション再起動。
  - A/B でリンギング/シャープ過多/ゴーストを確認。
  - 性能: 補正 ON/OFF のフレーム時間、GPU 時間、WiVRn のエンコード負荷。
- **回帰**: 補正 OFF がデフォルト。既存の動的解像度/輝度/オーバーレイが壊れないこと。

---

## 6. 代替アーキテクチャ

| 案 | 内容 | 利点 | 欠点 | 向いているケース |
|---|---|---|---|---|
| A. XRizer の Submit 内補正（推奨） | ゲームの眼テクスチャを OpenXR スワップチェーンへ描く前に補正 | 実装が最も自然。ドライバ/MinHook 不要。オーバーレイ保護が構造的 | XRizer 経由のゲームのみ。最終合成画像ではない | 本フォークの目的（VRChat on Linux/WiVRn） |
| B. OpenXR API レイヤー | `xrEndFrame` を横取りし、アプリのレイヤー/テクスチャを補正 | ネイティブ OpenXR アプリにも適用可能。XRizer に依存しない | レイヤーのインストール、デバイス/キュー管理、合成順序の理解が必要。最終画像は依然取得不可 | システム全体に近い形で適用したい |
| C. Monado/WiVRn コンポジタ統合 | ランタイムの最終合成パスで補正 | TrueFovea に最も近い。オーバーレイ含む最終画像を扱える。全アプリに適用 | ランタイムごとの C/C++ 改造。配布がランタイム依存。工数大 | 最終画像レベルの補正を追求する |
| D. SteamVR ドライバ | TrueFovea と同じ DirectMode ラップ | SteamVR 全体に適用。最終眼テクスチャを扱える | Windows/SteamVR 専用。MinHook/ドライバ保守。XRizer とは別プロジェクト | Windows/SteamVR が主対象 |
| E. ミラー + 投影オーバーレイ | TrueFovea 1.0.3 方式 | ゲームの解像度を変えずに確認できる | OpenXR 標準に mirror がない。ランタイム固有 API 依存 | 診断/投影確認のみ |

**結論**: 本フォークの目的（Linux/WiVRn で VRChat を動かす）には **A** が最適。最終画像レベルの画質や全アプリ適用を求めるなら **C**、Windows/SteamVR なら **D**、ネイティブ OpenXR も含めるなら **B** を検討する。

---

## 7. 困難・不可能な点と代替案

| 項目 | なぜ困難/不可能か | 代替案 |
|---|---|---|
| 最終合成画像の補正 | XRizer はコンポジタではなく、OpenXR 標準に最終画像の readback がない（`src/compositor.rs:412-431`） | ゲームレイヤーを補正（A）。最終画像が必要なら Monado/WiVRn コンポジタ（C） |
| 全 SteamVR アプリへの適用 | XRizer は SteamVR を置き換えるクライアント。`vrserver.exe` を持たない | OpenXR API レイヤー（B）または SteamVR ドライバ（D） |
| DirectMode / MinHook / ドライバ登録 | XRizer に対象となる HMD ドライバ/`vrserver.exe` がない | 不要。`IVRCompositor::Submit` を直接処理 |
| ミラー取得（1.0.3 方式） | OpenXR 標準に mirror API がない | ゲームレイヤー補正、またはランタイム固有 API |
| Windows / D3D11 | 現行バックエンドは Vulkan/OpenGL のみ（`src/graphics_backends.rs:62-70`, `:120-141`） | D3D11 バックエンド追加（大工数）、または SteamVR ドライバ（D） |
| TrueFovea の学習重み | 重みは非公開・独自部分。`TERMS.txt` が逆コンパイル/組み込みを禁止 | FSR 1（MIT）または自前訓練 |
| 時間補正 | モーションベクタ/深度がなく、複数パスのブロックマッチングが必要。ゴースティング/遅延の検証コスト大 | 空間補正のみ、軽量再投影、ランタイムの frame synthesis |
| 設定アプリの完全移植 | Win32/トレイ/自動起動/ドライバ登録は XRizer の構造に合わない | JSON 設定 + 外部 GUI + OpenVR Settings API |
| 共有メモリ IPC | プロセス境界がない | 不要。外部ツール連携時のみ Unix socket/共有メモリ |
| 外部オーバーレイの強化 | XRizer は外部オーバーレイアプリを主対象にしていない（`README.md:53`） | ゲームレイヤーのみ補正。オーバーレイはランタイムに任せる |
| HMD 固有の投影経路 | XRizer は HMD ドライバを持たず、OpenXR ランタイムが抽象化 | ランタイム/ドライバ側で実装 |

---

## 8. ライセンス・配布面の注意

### 8.1 XRizer 本体

- XRizer は **GPL-3.0-or-later**（`Cargo.toml:6`, `LICENSE`）。追加コードを配布する場合、GPL 互換ライセンスでソースを提供する必要がある。
- 既存の `openvr` クレート/ヘッダのライセンス（OpenVR）を維持する。
- ビルド時に `glslc` を使用する（`shaders/src/lib.rs:59-70`）。`glslc` 自体は配布物に含めない限り問題ないが、ビルド手順に明記する。

### 8.2 FSR 1 を使う場合

- FSR 1 は **MIT License**（Copyright (c) 2021 Advanced Micro Devices, Inc.）。MIT は GPL-3.0 と互換。
- バイナリ/ソースに MIT の著作権表示とライセンス全文を含める。
- MIT は特許権の明示的許諾ではない。商用配布や大規模配布では、必要に応じて法的確認を行う。
- FSR 1 の GLSL/HLSL 実装をそのままコピーする場合は、AMD の著作権表示を保持する。自前実装に置き換える場合も、アルゴリズムの出典を明記するのが望ましい。

### 8.3 TrueFovea の資産

- `TERMS.txt` は、TrueFovea の独自部分について以下を禁止している。
  - 再配布、転載、販売、貸与、ライセンス共有
  - 逆アセンブル、逆コンパイル、内部実装を解析・抽出するリバースエンジニアリング
  - 独自部分を複製・改変して別の製品・サービスへ組み込む行為
- したがって、**TrueFovea の HLSL シェーダ、定数、学習重み、UI 資産、ファイルフォーマット、内部構造を XRizer にコピーしてはいけない**。`ANALYSIS.md` は挙動理解のための参考資料として扱い、実装は仕様レベルの独立実装にする。解析レポート自体を配布物や公開リポジトリに同梱しない。
- 正式な clean-room を主張するなら、解析レポートを見ていない実装者に公開仕様だけを渡す分離が必要。同一開発者が解析レポートを見て実装する場合、法的には「独立実装」と主張しにくい可能性がある。配布前に専門家へ相談することを推奨する。
- TrueFovea の名称/ロゴ/UI を模倣しない。README でも「TrueFovea 互換/提携」と誤認させる表現を避ける。

### 8.4 VRChat / SteamVR / アンチチート

- XRizer はゲームファイルを変更しないが、ゲームプロセス内に `vrclient.so` としてロードされる。TrueFovea のようにゲームプロセス外で完結するわけではない。
- VRChat の利用規約、EAC の扱い、SteamVR/OpenXR ランタイムの規約は別途確認する。本レポートは法的判断をしない。
- OSC で受け取る視線データは機微情報。既定でローカルバインド、LAN OFF、ログ出力なしとする。
- 診断情報に画像・視線座標・生 OSC・アバター名・シリアル・IP・絶対パスを含めない（TrueFovea の診断方針 `ANALYSIS.md` §3.11 と同等）。

### 8.5 配布チェックリスト

- [ ] GPL-3.0-or-later のソース提供（改変版を含む）
- [ ] FSR 1 を使う場合、MIT ライセンスと著作権表示を同梱
- [ ] OpenVR ヘッダ/ライブラリのライセンス表示
- [ ] TrueFovea のコード/シェーダ/重み/UI 資産を含めない
- [ ] README に「TrueFovea の公認・提携ではない」と明記
- [ ] プライバシー: OSC は既定 localhost、LAN OFF、テレメトリなし
- [ ] 設定ファイルの場所と Proton 共有方法を記載
- [ ] 既知の制約（最終画像ではない、XRizer 経由のみ、Vulkan/GL のみ、MSAA/配列の制限）を記載

---

## 9. リスクと未検証事項

1. **実機未検証**: 本調査は静的。Vulkan パイプラインの実際の描画、WiVRn での性能、VRChat のテクスチャフォーマットは未確認。
2. **性能**: フルスクリーンのアップスケール + シャープは、GPU/解像度/FSR の有無で大きく変わる。特に WiVRn のエンコード負荷との相互作用は実測が必要。概算では 0.3〜1.5 ms/フレーム程度だが、保証しない。
3. **画質**: ランタイムの歪み/色収差/リプロジェクションの前に補正するため、TrueFovea の最終画像補正と同一にはならない。テキスト/細線のリンギング、半透明の境界、UI の過剰シャープを実機で確認する。
4. **OpenXR eye gaze**: ランタイム/HMD/許可に依存。WiVRn の Pico 系 eye tracking は対応が進んでいるが、全環境ではない。取得できない場合のフォールバックを必須にする。
5. **OSC のネットワーク**: Proton コンテナ内でホストの UDP を受け取れるかは環境依存。必要ならホスト側デーモン。
6. **フォーマット**: MSAA、配列テクスチャ、linear color space、BC 圧縮、HDR の組み合わせ。MVP では一部フォールバック。
7. **`SubmitWithArrayIndex`**: 既存の `todo!()`。配列テクスチャを使うゲームでは補正以前に問題になる。必要なら同時に実装。
8. **回帰**: 補正 OFF を既定にし、既存の動的解像度/輝度/オーバーレイに影響を与えない。`is_usable_swapchain` の変更は慎重にテストする。
9. **保守**: OpenXR ランタイム/ゲーム/Proton の更新で挙動が変わる。ログとフォールバックを充実させる。
10. **法的**: TrueFovea の Terms、VRChat/EAC、FSR の特許。必要に応じて専門家確認。

---

## 10. 判断が必要な点

以下は、実装着手前に決めると変更範囲が大きく変わる点である。

1. **対象プラットフォーム**: 現行の Linux / WiVRn / Monado のままでよいか。Windows / SteamVR も対象にするなら、XRizer ではなく SteamVR ドライバ（TrueFovea と同系統）または OpenXR API レイヤーを検討すべき。
2. **必須スコープ**: MVP は「固定中央/全体の空間補正」でよいか。視線追従（OpenXR/OSC）、時間補正、学習ネットワーク、設定 GUI のどこまでを必須にするか。
3. **適用範囲**: XRizer 経由のゲームだけでよいか。SteamVR 全体/ネイティブ OpenXR アプリにも適用したいなら、アーキテクチャが変わる（OpenXR レイヤー/ランタイム改造）。
4. **視線ソース**: eye tracking ハードウェアはあるか。OpenXR eye gaze が使える環境か、OSC/VRCFT フォールバックでよいか。
5. **設定 UI**: JSON 設定 + 外部 GUI でよいか。HMD 内の設定パネル/キャリブレーション UI まで必要か。
6. **ライセンス**: GPL-3.0-or-later のままでよいか。FSR 1（MIT）の利用を許容するか。TrueFovea のコード/シェーダ/重みを一切コピーしない方針でよいか。
7. **性能目標**: WiVRn のスタンドアロン（Quest/Pico）と PCVR のどちらを重視するか。目標解像度/Hz/GPU を決めると、FSR 1 を入れるか、Fast パスのみにするかが変わる。

---

## 11. 根拠一覧

### 11.1 TrueFovea

- `ANALYSIS.md` §0（結論）、§2（全体像）、§3.1（OpenVR 初期化）、§3.3（ドライバ/MinHook）、§3.4（DirectMode ラップ）、§3.5（GPU 補正）、§3.6（IPC）、§3.7（解像度）、§3.8（視線）、§3.9（OSC）、§3.10（オーバーレイ）、§3.11（診断）、§4（b01）、§5（1.0.3 ミラー）。
- `True-Fovea-for-VRChat-1.0.2/.../TERMS.txt`（逆アセンブル/組み込み禁止）。
- `True-Fovea-for-VRChat-1.0.2/.../README.md`（対象、視線、OSC、倍率、オーバーレイ）。

### 11.2 XRizer

- `README.md:5`（OpenVR on OpenXR）、`:53`（標準アプリ対象、オーバーレイ/ユーティリティ/背景は主対象外）、`:138-175`（動的解像度）、`:177-189`（輝度）。
- `README.ja.md:65-126`（フォーク機能、Arch/WiVRn、Proton 共有）。
- `src/compositor.rs:738-823`（Submit）、`:1028-1165`（FrameController/スワップチェーン）、`:1225-1304`（submit_impl）、`:1306-1432`（end_frame）、`:412-431`（ミラー未実装）、`:714-723`（SubmitWithArrayIndex 未実装）。
- `src/graphics_backends.rs:10-60`（trait）、`:62-70`（Vulkan/GL）、`:120-141`（対応テクスチャ）。
- `src/graphics_backends/vulkan.rs:202-353`（コピー）、`:355-363`（flush）、`:365-565`（オーバーレイパス）、`:728-1001`（PipelineData）、`:1005-1031`（フォーマット）。
- `src/graphics_backends/gl.rs:225-299`（GL コピー）。
- `src/openxr_data.rs:46-48`（推奨サイズ）、`:128-155`（拡張）、`:433-461`（SessionData）、`:593-637`（スワップチェーン/フォーマット）。
- `src/system.rs:198-229`（推奨サイズ/スケール）、`:230-272`（投影）、`:478-493`（視線スタブ）。
- `src/settings.rs:9-68`（scale ファイル）、`:86-122`（brightness ファイル）、`:193-218`（SetFloat）。
- `src/overlay.rs:164-333`（レイヤー）、`:505-609`（オーバーレイテクスチャ）。
- `src/clientcore.rs:119-137`（Scene/Background）。
- `src/applications.rs:15-180`（未実装が多い）。
- `src/input/action_manifest.rs:114-176`（内部アクションセット）、`src/input/legacy.rs:91-101`（attach/sync）。
- `shaders/src/lib.rs:13-17`（シェーダ一覧）、`shaders/src/overlay.frag`（既存の単純コピー）。
- `openvr/headers/openvr-2.15.6.h:2564-2569`（`GetEyeTrackedFoveationCenter`）。
- `Cargo.toml:6`（GPL-3.0-or-later）。

### 11.3 外部資料

- OpenXR `XR_EXT_eye_gaze_interaction`（instance extension）: <https://registry.khronos.org/OpenXR/specs/1.1/man/html/XR_EXT_eye_gaze_interaction.html>
- AMD FidelityFX Super Resolution 1（MIT、EASU/RCAS）: <https://gpuopen.com/fidelityfx-superresolution/>
- OpenXR には標準のコンポジタ readback/mirror がない旨の議論:
  - <https://community.khronos.org/t/support-for-reading-back-the-compositor-output/108853>
  - <https://steamcommunity.com/app/250820/discussions/8/3417685728361875720/>
- WiVRn の eye tracking / foveation 関連 issue・changelog（実装状況はバージョン依存）: <https://github.com/WiVRn/WiVRn/issues/1163>
- VRChat OSC eye tracking のアドレス一覧: <https://docs.vrchat.com/docs/osc-eye-tracking>

---

## 12. 付録: 用語

- **DirectMode**: HMD ドライバがコンポジタへ直接テクスチャを渡す SteamVR の表示経路。`IVRDriverDirectModeComponent_008/009`。
- **SubmitLayer / Present**: DirectMode の眼テクスチャ提出と提示。
- **MinHook**: x86/x64 のインラインフックライブラリ。TrueFovea は `TrackedDeviceAdded` の 1 箇所だけをフックする。
- **OpenXR composition layer**: アプリがランタイムに渡すレイヤー（projection / quad / cylinder / equirect2）。XRizer は projection + overlay を送る。
- **EASU / RCAS**: FSR 1 の空間アップスケールとシャープニング。
- **VRCFT**: VRCFaceTracking。アバター用の目の値から視線を推定する外部ツール。
- **OSC**: Open Sound Control。UDP 上のメッセージ/バンドル。VRChat のアバター/視線入力に使われる。
