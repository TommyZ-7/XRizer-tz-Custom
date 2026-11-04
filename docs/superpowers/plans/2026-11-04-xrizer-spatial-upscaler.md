# XRizer Experimental Spatial Upscaler Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an experimental, default-off, Vulkan-only spatial upscaler to XRizer's OpenVR `Submit` path. The programme is staged as **PoC → FSR 1 → MLP infrastructure → hybrid residual CNN → SRPO**, with Fast/Balanced/FSR/ML presets switchable from a neutral config file. The base resampler is always applied to the whole frame (FSR 1 EASU, Snapdragon GSR 1 / SGSR 1, or bilinear). Enhancement is controlled by a **continuous radial weight field** (`center`, `radius`, `feather`, `strength`): ML residual and RCAS/sharpen strength are multiplied by the per-pixel weight, so regions where the weight falls to zero keep only the base resample. The main ML target is a hybrid: a classical base plus a tiny INT8 CNN (QuickSRNet/FSRCNN class, 10–30k params) predicting a clamped, weighted residual, trained with QAT. A per-pixel MLP is the first inference baseline, not the final form. DP4A (`VK_KHR_shader_integer_dot_product`) is the primary inner loop; cooperative matrix (`VK_KHR_cooperative_matrix`) is an experimental comparison decided by an on-target microbenchmark.

**Architecture:** Intercept `IVRCompositor::Submit`, upscale each eye texture into an OpenXR swapchain sized to the runtime's recommended view size, then submit the corrected projection layer as usual. Base pass (EASU / SGSR 1 / bilinear) is full-frame. Optional ML pass computes a clamped residual only over the bounding box of the effective weight region and writes it weighted. Final pass applies RCAS with `strength × weight`. There is no centre/periphery zone split and no feathered zone composite; the weight is continuous. Overlays remain separate OpenXR layers and are never sharpened. Both eyes use identical weights, tile geometry, and deterministic processing.

**Tech Stack:** Rust 2024, `ash`/Vulkan, OpenXR, GLSL (`glslc`), `serde_json`, PyTorch only for offline training (not a runtime dependency), existing `fakexr` test harness.

**Spec:** `XRIZER_TRUEFOVEA_FEASIBILITY.md` (repository root), especially §4.1, §4.2 (spatial weighting), §4.6, §4.7, §5.1–§5.5. Prior-art decisions come from `/home/tommyz7/work-saeki/research/vr-spatial-ml-upscaler-prior-art.md` and its sources file: FSR 1 §1.1, SGSR 1 §1.7, QuickSRNet/FSRCNN §2.1, SRPO §2.1, DP4A vs cooperative matrix §4.1, training §5, and the shortlist in §6. FSR 2+ remains out of scope because XRizer has no motion vectors or depth.

## Spatial Weight Field

The spatial logic is a continuous field, not zones:

```text
d = length((uv - center) * vec2(dst_width / dst_height, 1.0))
w = 1 - smoothstep(radius, radius + feather, d)
```

- `center` is the field centre in **pre-distortion eye-texture UV space** (default `[0.5, 0.5]`).
- `radius` is the full-weight radius; `feather` is the falloff width; `strength` scales enhancement.
- `w` multiplies **ML residual**, **RCAS strength**, and SGSR 1 adaptive-sharpening strength. The base resample is never faded.
- **Full mode is not a separate mode.** It is the same field with a large radius, e.g. `radius = 2.0` (larger than any aspect-corrected corner distance), so `w = 1` across the frame.
- ML dispatch range is the bounding box of pixels with `w > epsilon`; outside that box the base image is untouched. This keeps ML cost equivalent to the previous centre-region plan when `radius` is moderate. A full-radius ML setting intentionally costs much more; the runtime budget/kill-switch applies.
- Exact geometric mapping to a projection plane / visual-angle space is a **future refinement**. The baseline uses radial distance in pre-distortion eye-texture space; note this in docs and training metadata.

## Global Constraints

- Execution method: **Native** (one session implements the plan; final review after the branch). No subagent implementation unless the user changes this decision.
- Target platform: Linux + WiVRn/Monado only. No Windows, D3D11/D3D12, SteamVR driver, or shared-memory driver IPC.
- Scope: games launched through XRizer only. No system-wide OpenXR API layer or runtime compositor changes.
- Feature is **default off**. With the default config, existing behavior must be unchanged: no extra pipeline creation and no swapchain size changes.
- Config file is **`~/.config/xrizer/upscaler.json`** (user-confirmed). Config keys use neutral `upscaler` naming, not reference-software terminology.
- Output swapchain size is `runtime_recommended_size × output_scale`, clamped to at least 1×1. Default `output_scale = 1.0`.
- Presets: `fast`, `balanced`, `fsr`, `ml`. Default `balanced`.
  - `fast`: bilinear base + weighted simple sharpen.
  - `balanced`: SGSR 1 full-frame base; its adaptive sharpening is scaled by `w`.
  - `fsr`: FSR 1 EASU full-frame base + weighted RCAS.
  - `ml`: FSR 1 EASU full-frame base + weighted ML residual + weighted RCAS.
  - SGSR 1 is a **full-frame base filter option**. Balanced uses it; FSR/ML default to EASU. A future config may expose the base filter independently.
- Default config: `enabled=false`, `preset=balanced`, `center=[0.5,0.5]`, `radius=0.35`, `feather=0.10`, `strength=1.0`, `output_scale=1.0`, `preserve_alpha=true`, `compare=false`, `show_region=false`, `model_path=null`.
- Clamps: `center` 0.0–1.0 per axis; `radius` 0.05–2.0; `feather` 0.0–1.0; `strength` 0.0–3.0; `output_scale` 0.5–2.0.
- Full-frame weight example: `radius=2.0`, `feather=0.0` (or any radius above the corner distance). Fixed-centre example: `center=[0.5,0.5]`, `radius=0.35`, `feather=0.1`.
- Alpha threshold is `alpha < 0.999`; those pixels must be copied without sharpening/ML.
- A/B interval is 2 seconds. A/B only toggles enhancement, never the output extent or base resample.
- ML preset is allowed in fixed or full weight configurations; it is not forced to FSR. Full-radius ML is allowed but expensive; budget/kill-switch applies.
- ML default hidden/channel size is **16**, with a model manifest that also accepts **8**. The config schema and pipeline interface must not change when swapping 8↔16.
- **ML main target:** hybrid residual CNN. A tiny INT8 CNN takes the EASU/classical base and predicts a **clamped residual**, weighted by `w`; it is not a free RGB generator. 10–30k parameters, QuickSRNet/FSRCNN-family topology, QAT. The per-pixel MLP is only the first inference baseline.
- **Established fact:** DXVK (including v3.1.1 shipped with GE-Proton 11-11) does **not** enable `shaderIntegerDotProduct` on the Vulkan device it creates. RADV/the RX 7900 XTX supports it, but the borrowed device does not have it enabled. Therefore VRChat cannot use DP4A without an explicit device-creation intervention.
- **Required intervention:** ship an **XRizer Vulkan implicit layer** that intercepts `vkCreateDevice`, and only when the physical device supports it, adds `shaderIntegerDotProduct` (`VK_KHR_shader_integer_dot_product`) and, for the experiment, `VK_KHR_cooperative_matrix` plus its dependencies (`shaderInt8`, `storageBuffer8BitAccess`, Vulkan memory model, etc.) to the game/DXVK device. The layer is **opt-in** via `XRIZER_UPSCALER_VK_LAYER=1`; `XRIZER_UPSCALER_VK_LAYER_DISABLE=1` is the mandatory off-switch. It must be usable from Steam launch options and WiVRn session environment.
- **Layer fail-open:** if the physical device does not support a feature, or the pNext chain is not safely editable, or the layer itself fails, it must pass the original `VkDeviceCreateInfo` through unchanged. If a modified `vkCreateDevice` fails, retry once with the original create info. The game must start even when the layer is broken. Never require the layer for non-ML paths.
- **Inner-loop priority:** with the layer active, DP4A (`VK_KHR_shader_integer_dot_product` / Vulkan 1.3 `shaderIntegerDotProduct`) is the primary path for VRChat. Cooperative matrix (`VK_KHR_cooperative_matrix`, RDNA3 WMMA 16×16×16 int8) is an experimental comparison also enabled through the layer. An on-target microbenchmark decides whether coopmat is worth keeping. Do not architect around coopmat before measuring DP4A.
- **Fallback scope:** fallback to `fsr`/`balanced` is for environments where the layer is not enabled, the device/driver does not support the feature, or the layer declines to modify the device. It is no longer the expected VRChat path. Probe actual device capabilities at runtime; do not assume the layer succeeded.
- **Budget:** aim for ≤1.0 ms stereo total for the upscaler, ≤0.5 ms typical, at 72–120 Hz. Add a runtime kill-switch: if the ML dispatch exceeds budget (GPU timestamps) or a required feature is missing, fall back to FSR 1/SGSR 1. Avoid extra full-screen intermediate copies; fuse where possible. ML dispatch is restricted to the weight bounding box.
- **Temporal/stereo:** both eyes must use identical weights, identical tile geometry, and deterministic processing. No per-eye adaptive state. Residual clamping is mandatory. No GAN loss. No engine history/motion vectors/depth.
- **Training:** captures are user-provided later. Implement training/evaluation tooling now; run actual training only after captures arrive (Task 14). Do not commit captures or user-derived weights.
- **Licences:** XRizer and the new Vulkan layer are GPL-3.0-or-later. FSR 1 MIT; SGSR 1 BSD-3; AMD/Qualcomm notices preserved. SGSR 1 is a full-frame base filter option. Do not copy TrueFovea code, HLSL, constants, weights, UI assets, or file formats.
- Every Rust task ends with `cargo test`; shader/ML tasks end with `cargo xbuild` plus manual HMD checks. Final task runs `cargo clippy --workspace --all-targets` and `cargo fmt --check`.

## Review Focus

These are the failure modes most likely to bite a user, and where their tests live:

1. **ML inner loop/device feature and layer safety**: DXVK does not enable `shaderIntegerDotProduct`; the opt-in layer must add it only when supported, fail open on unknown chains, and never prevent device creation. If the layer is absent/broken, DP4A/coopmat must probe and fall back. Tested by layer plan/probe tests (Task L2) and `choose_inner_loop`/capability tests (Task 10).
2. **Continuous weight field**: weight must be smooth, centred correctly, and go to zero outside `radius+feather`; full mode must be represented by a large radius, not a separate branch. Tested by `weight_field_*` tests (Task 1) and manual gradient checks (Tasks 4/6/7).
3. **Temporal stability/stereo consistency**: residual CNNs and sharpening can shimmer or differ per eye. Tested by `residual_clamp`/deterministic-processing tests (Tasks 9, 13) and tLPIPS/video evaluation after captures (Task 14).
4. **Bounds/flip, alpha, and gamma**: inverted/cropped bounds, alpha <0.999, and sRGB/linear handling must be preserved for the base and enhancement. Tested by `source_uv_respects_bounds_and_flip` (Task 1), `preserve_alpha_is_in_flags` (Task 5), and manual colour checks (Tasks 6/7).
5. **Resolution/model fallback**: dynamic render scale must not resize the output swapchain; missing/invalid/unsupported ML must fall back to FSR/SGSR. Tested by `exact_swapchain_reuse_requires_same_size` and model/fallback tests (Tasks 4, 8, 11).

---

## File Structure

- **Create `vulkan-layer/`** — Rust `cdylib` workspace member `xrizer-vulkan-layer` implementing the opt-in implicit layer: `Cargo.toml`, `src/lib.rs` (layer exports, `vkCreateDevice` interception, feature-chain editor), `manifest.json.in`.
- **Create `scripts/install-vulkan-layer.sh`** — install/remove the manifest + `.so` in `$XDG_DATA_HOME/vulkan/implicit_layer.d` (or print portable env usage).
- **Create `vulkan-layer-probe/`** — small Rust bin crate `xrizer-vk-probe` that creates a device with the layer and checks DP4A/coopmat pipeline creation; used for manual/integration verification.
- **Modify `xbuild`** — build the layer and copy `libxrizer_upscaler_layer.so` + manifest into the runtime directory.
- **Create `src/upscaler/mod.rs`** — config, presets, hot reload, parameter/fallback selection.
- **Create `src/upscaler/math.rs`** — continuous weight field, source UV, luma patch, residual clamp, ML dispatch bounds; CPU reference tests.
- **Create `src/upscaler/model.rs`** — neutral `XRU1` model format for MLP/residual CNN/SRPO, manifest validation, int8 weights/scales.
- **Create `src/upscaler/inner_loop.rs`** — DP4A/coopmat capability model and pure selection functions.
- **Create `src/upscaler/auto_scale.rs`** — simple frame-time controller.
- **Modify `src/lib.rs`** — register `mod upscaler;`.
- **Modify `src/openxr_data.rs`** — `base_recommended_view_size()`.
- **Modify `src/system.rs`** — reuse base size and `apply_render_scale()`.
- **Modify `src/settings.rs`** — `apply_render_scale()`, scale get/set helpers.
- **Modify `src/graphics_backends.rs`** — `supports_upscaler()`, `supports_ml_upscaler()`, `Option<&UpscalerParams>` on `copy_texture_to_swapchain`.
- **Modify `src/graphics_backends/vulkan.rs`** — hook up intermediate images and the new `vulkan/upscaler.rs` module.
- **Create `src/graphics_backends/vulkan/upscaler.rs`** — Fast/SGSR/FSR/ML pipelines, intermediate textures, source views, push constants, fallbacks, timestamps.
- **Modify `src/graphics_backends/gl.rs`** — update trait signature, report no upscaler support, keep old path.
- **Modify `src/compositor.rs`** — output extent, parameter plumbing, exact swapchain reuse, A/B, diagnostics, fake-backend updates.
- **Create `shaders/src/upscaler.vert`** — full-screen triangle.
- **Create `shaders/src/fast.frag`** — Fast path (bilinear base + weighted sharpen).
- **Create `shaders/src/sgsr1.frag`** — SGSR 1 full-frame base (BSD-3), adaptive sharpening scaled by weight.
- **Create `shaders/src/fsr_easu.frag`** — FSR 1 EASU full-frame base (MIT).
- **Create `shaders/src/enhance.frag`** — weighted RCAS final pass (no zone composite).
- **Create `shaders/src/mlp_dp4a.comp`** — per-pixel MLP baseline via DP4A.
- **Create `shaders/src/ml_bench.comp`** — DP4A vs coopmat 16→16 microbenchmark.
- **Create `shaders/src/residual_cnn.comp`** — hybrid residual CNN (DP4A or coopmat chosen by benchmark).
- **Create `shaders/src/srpo.comp`** — SRPO-style offset/kernel prediction (later phase).
- **Modify `shaders/src/lib.rs`** — compile all new shaders.
- **Create `tools/train_upscaler.py`** — offline training/QAT/export for MLP, residual CNN, SRPO.
- **Create `tools/eval_temporal.py`** — tLPIPS/warping error/IF-SSIM/temporal variance evaluation.
- **Create `tools/requirements-upscaler.txt`** — PyTorch/NumPy/Pillow/flow dependencies.
- **Create `tools/upscaler_dataset.md`** — capture layout, LR generation, AA/colour metadata, privacy.
- **Modify `README.md` and `README.ja.md`** — config, presets, model setup, limitations, licences.
- **Create `docs/upscaler-benchmark.md`** — benchmark and temporal results (Task 14).

---

## Phase 0: PoC and Foundation

### Task 1: Upscaler Config, Parameters, and Weight Math

**Files:**
- Create: `src/upscaler/mod.rs`, `src/upscaler/math.rs`
- Modify: `src/lib.rs`
- Test: inline tests

**Interfaces:**
- `UpscalerPreset { Fast, Balanced, Fsr, Ml }`
- `UpscalerConfig { enabled, preset, center, radius, feather, strength, output_scale, preserve_alpha, compare, show_region, model_path }`
- `UpscalerParams { dst_width, dst_height, center, radius, feather, strength, src_bounds, preset, preserve_alpha, show_region, enabled }` (`Clone, Copy, Debug, PartialEq`)
- `UpscalerConfig::{default, sanitized, params_for_size, params_for_size_at, params}`
- `UpscalerParams::{source_uv, weight_at, shader_kind, flags, with_fallback, ml_bounds}`
- `snapshot()`, `maybe_reload()`, `load_from_path()`, `candidate_paths()`, `compare_enabled()`
- `#[cfg(test)] test_lock()`, `reset_for_test()`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn default_is_disabled() {
    let c = UpscalerConfig::default();
    assert!(!c.enabled);
    assert_eq!(c.preset, UpscalerPreset::Balanced);
    assert_eq!(c.center, [0.5, 0.5]);
    assert_eq!(c.radius, 0.35);
    assert_eq!(c.feather, 0.10);
    assert_eq!(c.strength, 1.0);
    assert!(c.preserve_alpha);
}

#[test]
fn sanitize_clamps() {
    let c = UpscalerConfig {
        center: [-1.0, 2.0], radius: 0.0, feather: 9.0,
        strength: 99.0, output_scale: 9.0, ..Default::default()
    }.sanitized();
    assert_eq!(c.center, [0.0, 1.0]);
    assert_eq!(c.radius, 0.05);
    assert_eq!(c.feather, 1.0);
    assert_eq!(c.strength, 3.0);
    assert_eq!(c.output_scale, 2.0);
}

#[test]
fn params_none_when_disabled() { /* ... */ }

#[test]
fn params_output_size_uses_scale() {
    let b = vr::VRTextureBounds_t { uMin: 0.0, vMin: 0.0, uMax: 1.0, vMax: 1.0 };
    let c = UpscalerConfig { enabled: true, output_scale: 0.7, ..Default::default() };
    let p = c.params_for_size(1000, 800, b).unwrap();
    assert_eq!((p.dst_width, p.dst_height), (700, 560));
}

#[test]
fn source_uv_respects_bounds_and_flip() {
    let b = vr::VRTextureBounds_t { uMin: 0.1, vMin: 0.9, uMax: 0.9, vMax: 0.1 };
    let c = UpscalerConfig { enabled: true, ..Default::default() };
    let p = c.params_for_size(100, 100, b).unwrap();
    assert_eq!(p.source_uv([0.0, 0.0]), [0.1, 0.9]);
    assert_eq!(p.source_uv([1.0, 1.0]), [0.9, 0.1]);
}

#[test]
fn weight_field_center_radius_feather() {
    let b = vr::VRTextureBounds_t { uMin: 0.0, vMin: 0.0, uMax: 1.0, vMax: 1.0 };
    let c = UpscalerConfig { enabled: true, center: [0.5, 0.5], radius: 0.2, feather: 0.1, ..Default::default() };
    let p = c.params_for_size(100, 100, b).unwrap();
    assert_eq!(p.weight_at([0.5, 0.5]), 1.0);
    assert_eq!(p.weight_at([0.7, 0.5]), 1.0);
    assert!((0.0..1.0).contains(&p.weight_at([0.75, 0.5])));
    assert_eq!(p.weight_at([0.9, 0.5]), 0.0);
}

#[test]
fn large_radius_makes_weight_one_everywhere() {
    let b = vr::VRTextureBounds_t { uMin: 0.0, vMin: 0.0, uMax: 1.0, vMax: 1.0 };
    let c = UpscalerConfig { enabled: true, radius: 2.0, feather: 0.0, ..Default::default() };
    let p = c.params_for_size(100, 100, b).unwrap();
    assert_eq!(p.weight_at([0.0, 0.0]), 1.0);
    assert_eq!(p.weight_at([1.0, 1.0]), 1.0);
}

#[test]
fn center_offset_moves_weight() {
    let b = vr::VRTextureBounds_t { uMin: 0.0, vMin: 0.0, uMax: 1.0, vMax: 1.0 };
    let c = UpscalerConfig { enabled: true, center: [0.25, 0.75], radius: 0.1, feather: 0.0, ..Default::default() };
    let p = c.params_for_size(100, 100, b).unwrap();
    assert_eq!(p.weight_at([0.25, 0.75]), 1.0);
    assert_eq!(p.weight_at([0.5, 0.5]), 0.0);
}

#[test]
fn ml_bounds_follow_weight_region() {
    let b = vr::VRTextureBounds_t { uMin: 0.0, vMin: 0.0, uMax: 1.0, vMax: 1.0 };
    let c = UpscalerConfig { enabled: true, radius: 0.2, feather: 0.1, ..Default::default() };
    let p = c.params_for_size(1000, 1000, b).unwrap();
    let (x, y, w, h) = p.ml_bounds(0.001);
    assert!(x > 0 && y > 0 && w < 1000 && h < 1000);
    assert!(x + w <= 1000 && y + h <= 1000);
}

#[test]
fn preset_maps_to_shader_kind() {
    // fast=0, balanced=1, fsr=2, ml=3
}

#[test]
fn compare_does_not_change_output_extent() { /* on/off same dst */ }

#[test]
fn json_roundtrip_and_bad_json_is_error() { /* ... */ }
```

`math.rs` tests: luma patch has 28 features (5×5 + phase 2 + scale 1), residual clamp helper, smoothstep agreement, ML bounds round/clamp.

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test upscaler:: -- --nocapture`  
Expected: FAIL.

- [ ] **Step 3: Implement**

- Serde values `fast|balanced|fsr|ml`.
- No `UpscalerMode`; fixed/full is represented by `radius` alone.
- `params_for_size_at` returns `None` if disabled; computes clamped `dst_*`, `center` (clamped 0–1), `src_bounds`, `enabled = compare_enabled(...)`.
- `params(base, bounds)` computes `base × output_scale`, `.round().max(1)`.
- `with_fallback(ml_supported)`: `Ml` + unsupported → `Fsr`; otherwise unchanged.
- `shader_kind`: fast=0, balanced=1, fsr=2, ml=3.
- `flags`: bit0 preserve alpha, bit1 show region, bit2 sharpening/ML enabled.
- `weight_at(uv)`:
  ```rust
  let aspect = dst_width as f32 / dst_height as f32;
  let dx = (uv[0] - center[0]) * aspect;
  let dy = uv[1] - center[1];
  let d = (dx*dx + dy*dy).sqrt();
  1.0 - smoothstep(radius, radius + feather, d)
  ```
  Handle `feather == 0.0` as a hard step; full frame is `radius = 2.0`.
- `ml_bounds(epsilon)`:
  - Effective radius `r = radius + feather`.
  - Convert `center ± (r / aspect, r)` to pixel coordinates, clamp to `[0, dst]`, return `(x, y, w, h)`.
  - If the region covers the frame, return full extent.
  - The ML dispatch uses this rect; outside remains base.
- `candidate_paths`: `$XRIZER_UPSCALER_CONFIG` → `$XDG_CONFIG_HOME/xrizer/upscaler.json` → `$HOME/.config/xrizer/upscaler.json` → `$XDG_RUNTIME_DIR/xrizer_upscaler.json` → `/tmp/xrizer_upscaler.json`.
- 500 ms hot reload; parse failure keeps previous config.
- Use neutral `upscaler` naming; no reference-software terminology.

- [ ] **Step 4: Run tests**

Run: `cargo test upscaler:: -- --nocapture`  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/upscaler src/lib.rs
git commit -m "feat(upscaler): add continuous weight-field config"
```

---

### Task 2: Base Recommended View Size and Scale Math

**Files:** `src/openxr_data.rs`, `src/system.rs`, `src/settings.rs`

**Interfaces:** `base_recommended_view_size()`, `apply_render_scale()`.

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn render_scale_rounds_and_clamps() {
    assert_eq!(apply_render_scale((1000, 800), 0.7), (700, 560));
    assert_eq!(apply_render_scale((1000, 800), 0.0), (100, 80));
    assert_eq!(apply_render_scale((1000, 800), 10.0), (4000, 3200));
    assert_eq!(apply_render_scale((1, 1), 0.1), (1, 1));
}
```

- [ ] **Step 2: Run to fail** — `cargo test render_scale_rounds_and_clamps`.

- [ ] **Step 3: Implement** — add helper; factor `GetRecommendedRenderTargetSize`; preserve existing scale polling/clamps.

- [ ] **Step 4: Run** — `cargo test render_scale_rounds_and_clamps`, `cargo test`.

- [ ] **Step 5: Commit**

```bash
git add src/openxr_data.rs src/system.rs src/settings.rs
git commit -m "feat(upscaler): expose base recommended view size"
```

---

## Phase 0.5: Opt-In Vulkan Implicit Layer

The layer is required to make DP4A usable in VRChat. It is deliberately opt-in, fail-open, and independent of the XRizer OpenVR runtime: it only edits device creation. Tasks L1/L2 must complete before Task 8; they can proceed in parallel with Tasks 3–7.

### Task L1: Layer Scaffold, Manifest, Build, and Installation

**Files:**
- Create: `vulkan-layer/Cargo.toml` (package `xrizer-vulkan-layer`; `[lib] name = "xrizer_upscaler_layer"`, `crate-type = ["cdylib"]`)
- Create: `vulkan-layer/src/lib.rs`
- Create: `vulkan-layer/manifest.json.in`
- Create: `scripts/install-vulkan-layer.sh`
- Create: `vulkan-layer-probe/Cargo.toml`, `vulkan-layer-probe/src/main.rs` (package `xrizer-vk-probe`)
- Modify: `Cargo.toml` (`members = ["xbuild", "vulkan-layer", "vulkan-layer-probe"]`)
- Modify: `xbuild/src/main.rs` (build/copy layer)
- Test: build/load checks; no behavior yet

**Interfaces:**
- Layer name: `VK_LAYER_XRIZER_upscaler`
- Enable env: `XRIZER_UPSCALER_VK_LAYER=1`
- Disable env: `XRIZER_UPSCALER_VK_LAYER_DISABLE=1`
- Exports: `xrizer_layer_GetInstanceProcAddr`, `xrizer_layer_GetDeviceProcAddr`, optional `vkNegotiateLoaderLayerInterfaceVersion`
- Manifest path in runtime: `vulkan/implicit_layer.d/xrizer_upscaler_layer.json`
- Library: `libxrizer_upscaler_layer.so`

- [ ] **Step 1: Add the layer crate and manifest**

`vulkan-layer/manifest.json.in` (installed form substitutes an absolute library path):

```json
{
  "file_format_version": "1.2.0",
  "layer": {
    "name": "VK_LAYER_XRIZER_upscaler",
    "type": "GLOBAL",
    "library_path": "./libxrizer_upscaler_layer.so",
    "api_version": "1.3.0",
    "implementation_version": "1",
    "description": "XRizer opt-in Vulkan feature-enablement layer",
    "functions": {
      "vkGetInstanceProcAddr": "xrizer_layer_GetInstanceProcAddr",
      "vkGetDeviceProcAddr": "xrizer_layer_GetDeviceProcAddr"
    },
    "enable_environment": { "XRIZER_UPSCALER_VK_LAYER": "1" },
    "disable_environment": { "XRIZER_UPSCALER_VK_LAYER_DISABLE": "1" }
  }
}
```

The layer crate is a `cdylib`; it must not link the Vulkan loader. Use raw Vulkan types from `ash`/`vk` and resolve the next layer/ICD through `VkLayerInstanceCreateInfo`/`VkLayerDeviceCreateInfo` as documented by the Vulkan Loader.

- [ ] **Step 2: Wire build and packaging**

- Add `vulkan-layer` to the workspace.
- Extend `xbuild` to build `xrizer-vulkan-layer`, copy `libxrizer_upscaler_layer.so` and the manifest into `<runtime>/vulkan/implicit_layer.d/`, and keep the manifest `library_path` relative for portable use.
- `scripts/install-vulkan-layer.sh --user`:
  - Copies the `.so` to `$XDG_DATA_HOME/xrizer/vulkan-layer/` (or `~/.local/share/xrizer/vulkan-layer/`).
  - Writes the manifest to `$XDG_DATA_HOME/vulkan/implicit_layer.d/xrizer_upscaler_layer.json` (fallback `~/.local/share/vulkan/implicit_layer.d/`) with an **absolute** `library_path`.
  - `--remove` removes only those files.
  - Does not require root.
- `scripts/install-vulkan-layer.sh --print-env` prints the portable Steam/WiVRn environment:
  ```sh
  XRIZER_UPSCALER_VK_LAYER=1
  VK_ADD_IMPLICIT_LAYER_PATH=/path/to/xrizer/vulkan/implicit_layer.d
  ```
  (`VK_ADD_IMPLICIT_LAYER_PATH` requires loader 1.3.296+; if unavailable, use `--user` install.)
- Steam Linux Runtime / pressure-vessel: prefer `scripts/install-vulkan-layer.sh --user` because pressure-vessel imports host implicit layers from the XDG data dirs. Avoid `VK_IMPLICIT_LAYER_PATH` (it can override and hide imported layers). Set only `XRIZER_UPSCALER_VK_LAYER=1` in the Steam launch options when the manifest is installed.
- WiVRn: export `XRIZER_UPSCALER_VK_LAYER=1` in the session environment; use `--user` install or `VK_ADD_IMPLICIT_LAYER_PATH` if the runtime/container sees that path.

- [ ] **Step 3: Verify layer discovery**

Run with a Vulkan tool:

```bash
XRIZER_UPSCALER_VK_LAYER=1 VK_LOADER_DEBUG=layer \
  VK_ADD_IMPLICIT_LAYER_PATH="$PWD/target/debug/vulkan/implicit_layer.d" \
  vulkaninfo --summary
```

Expected: loader log shows `VK_LAYER_XRIZER_upscaler` loaded. Without `XRIZER_UPSCALER_VK_LAYER=1`, the layer must not load. With `XRIZER_UPSCALER_VK_LAYER_DISABLE=1`, it must not load even if the enable env is set.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml xbuild vulkan-layer scripts/install-vulkan-layer.sh
git commit -m "feat(vk-layer): add opt-in implicit layer scaffold"
```

---

### Task L2: `vkCreateDevice` Feature Injection, Fail-Open, and Probe

**Files:**
- Modify: `vulkan-layer/src/lib.rs` (or split `features.rs`, `chain.rs`)
- Modify: `vulkan-layer-probe/src/main.rs` (package `xrizer-vk-probe`; created in Task L1)
- Modify: `scripts/install-vulkan-layer.sh`, README docs
- Test: pure plan tests in `vulkan-layer`; manual probe + VRChat/DXVK

**Interfaces:**
- `FeaturePlan { dot_product: bool, coopmat: bool, shader_int8: bool, storage_8bit: bool, memory_model: bool, extensions: Vec<&'static CStr> }`
- `build_feature_plan(physical_device, existing_chain) -> FeaturePlan`
- `rewrite_device_create_info(plan, original) -> Option<OwnedDeviceCreateInfo>`
- `vkCreateDevice` interception with `catch_unwind` and original-info retry.

- [ ] **Step 1: Write failing pure tests**

```rust
#[test]
fn supported_dot_product_adds_feature() { /* plan adds shaderIntegerDotProduct */ }

#[test]
fn unsupported_dot_product_leaves_chain_unchanged() { /* plan is empty */ }

#[test]
fn coopmat_requires_all_dependencies() {
    // shaderInt8 + storageBuffer8BitAccess + vulkanMemoryModel + cooperativeMatrix
    // missing any -> no coopmat extension/feature
}

#[test]
fn unknown_pnext_structure_fails_open() { /* None / original chain */ }

#[test]
fn existing_extensions_are_not_duplicated() { /* dedup by name */ }

#[test]
fn pEnabledFeatures_are_preserved_when_adding_features2() { /* ... */ }

#[test]
fn dxvk_like_chain_is_rewritten() {
    // Include the Vulkan11/12/13 + robustness2/custom-border-color structs
    // used by DXVK 3.1.1; the rewritten chain keeps them and enables dot product.
}
```

- [ ] **Step 2: Run to fail** — `cargo test -p xrizer-vulkan-layer`.

- [ ] **Step 3: Implement the edit plan**

- Intercept `vkCreateInstance` to initialize the next-layer instance dispatch chain; track `VkInstance -> VkPhysicalDevice` mappings by intercepting `vkEnumeratePhysicalDevices`; `vkCreateDevice` looks up the instance. If lookup fails, pass through and log.
- In `vkCreateDevice`:
  1. Find `VkLayerDeviceCreateInfo` in `pCreateInfo->pNext`; get next `vkGetInstanceProcAddr`/`vkGetDeviceProcAddr`; advance the layer link. Preserve all loader chain nodes (`VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO` and other loader-owned structs) in the rewritten chain; do not treat them as unknown application features.
  2. Query physical-device features with `vkGetPhysicalDeviceFeatures2` and a chain containing `VkPhysicalDeviceFeatures2`, `VkPhysicalDeviceVulkan12Features`, `VkPhysicalDeviceVulkan13Features`, `VkPhysicalDeviceCooperativeMatrixFeaturesKHR`, and `VkPhysicalDeviceShaderIntegerDotProductFeatures` (as available). If `vkGetPhysicalDeviceFeatures2`/Vulkan 1.1 is unavailable, pass through. Query device extensions.
  3. Build a `FeaturePlan` only from supported features:
     - DP4A: `shaderIntegerDotProduct`; add `VK_KHR_shader_integer_dot_product` if device API <1.3.
     - Coopmat: `cooperativeMatrix` + `shaderInt8` + `storageBuffer8BitAccess` + `vulkanMemoryModel` + documented extension dependencies (`VK_KHR_cooperative_matrix`, `VK_KHR_shader_float16_int8`/8-bit storage/memory-model as applicable). If any dependency is unsupported, skip coopmat.
  4. Copy the `pNext` chain into a bounded arena. Preserve loader-owned nodes; copy known application feature structures using a fixed `sType -> size` table covering the DXVK 3.1.1 feature set (Vulkan11/12/13, robustness2, custom border color, extended dynamic state, etc.). If any unknown **application** `sType` is encountered, abort the rewrite and call the next `vkCreateDevice` with the **original** `pCreateInfo`. If the real VRChat/DXVK probe hits this fallback, extend the table rather than weakening fail-open.
  5. If `pEnabledFeatures` is present, copy it into a `VkPhysicalDeviceFeatures2` in the rewritten chain (the features2 form controls device creation). Set `pEnabledFeatures = nullptr` in the copied `VkDeviceCreateInfo` only when features2 is present.
  6. Merge the planned fields into existing `VkPhysicalDeviceFeatures2`/`Vulkan12Features`/`Vulkan13Features`/`CooperativeMatrixFeaturesKHR` copies, or add new structs after the loader chain. Set `shaderInt8`, `storageBuffer8BitAccess`, `vulkanMemoryModel`, `shaderIntegerDotProduct`, and `cooperativeMatrix` only when supported.
  7. Copy and deduplicate `ppEnabledExtensionNames`; append only extensions that the physical device enumerates and that are not already present.
  8. Call the next `vkCreateDevice` with the rewritten info. If it returns any error, retry once with the original `pCreateInfo`. If the retry also fails, return the retry result. Log the feature additions and the retry.
  9. Wrap the interception in `catch_unwind`; on panic, call the next `vkCreateDevice` with the original info. The layer must never abort the game.
- Thread-safe dispatch/instance/device maps. No Vulkan loader link. No game file modification.

- [ ] **Step 4: Add the probe**

`vulkan-layer-probe/src/main.rs`:
- Creates an instance and picks the RX 7900 XTX.
- Creates a device with a DP4A compute pipeline; checks pipeline creation/execution.
- Optionally creates a coopmat 16×16×16 int8 pipeline when the layer enabled the feature.
- Prints `dp4a=ok/fallback`, `coopmat=ok/fallback`, and the layer env state.
- Exit code 0 if device creation succeeds (even if ML features are absent); ML-specific failures are reported, not fatal.

- [ ] **Step 5: Run tests and manual verification**

```bash
cargo test -p xrizer-vulkan-layer
cargo build -p xrizer-vulkan-layer
XRIZER_UPSCALER_VK_LAYER=1 VK_ADD_IMPLICIT_LAYER_PATH=... cargo run -p xrizer-vk-probe
```

Expected:
- Without the env: layer not loaded; probe reports DP4A unavailable (or game device as-is).
- With the env on RX 7900 XTX/RADV: probe reports DP4A available; coopmat reported if the device/driver supports it.
- With the env on an unsupported device: probe still starts, ML reports fallback; no crash.
- Force a bad rewrite (test hook) / unknown pNext: original device creation succeeds; log says fail-open.

Manual VRChat/DXVK:
```bash
XRIZER_UPSCALER_VK_LAYER=1 VK_ADD_IMPLICIT_LAYER_PATH=/path/to/xrizer/vulkan/implicit_layer.d \
  VR_OVERRIDE=/path/to/xrizer %command%
```
Expected: `VK_LOADER_DEBUG=layer` shows the layer; XRizer log shows DP4A available; `preset=ml` runs the DP4A baseline instead of falling back to FSR. If the layer is absent/disabled, ML falls back.

- [ ] **Step 6: Commit**

```bash
git add vulkan-layer vulkan-layer-probe scripts/install-vulkan-layer.sh README.md README.ja.md
git commit -m "feat(vk-layer): enable DP4A and coopmat on game devices"
```

---

## Phase 0 (continued): PoC Pipeline

### Task 3: Backend Capability and Pass-Through Pipeline (PoC)

**Files:** `src/graphics_backends.rs`, `src/graphics_backends/vulkan.rs`, new `src/graphics_backends/vulkan/upscaler.rs`, `src/graphics_backends/gl.rs`, `src/compositor.rs`, `shaders/src/upscaler.vert`, `shaders/src/upscaler.frag`, `shaders/src/lib.rs`

**Interfaces:**
- `supports_upscaler()`, `supports_ml_upscaler()`
- `copy_texture_to_swapchain(&mut self, ..., params: Option<&UpscalerParams>)`
- `UpscalerPipeline` pass-through skeleton

- [ ] **Step 1: Failing test** — fake backend opt-in test; `Fixture::set_fake_supports_upscaler`, `last_upscaler_params`; `FakeGraphicsData` records params.
- [ ] **Step 2: Run to fail** — `cargo test fake_backend_upscaler_is_opt_in`.
- [ ] **Step 3: Implement**:
  - Full-screen triangle + pass-through fragment shader.
  - `UpscalerPipeline`: pipeline/layout/render pass, **two descriptor sets** (left/right; both eye command buffers are submitted together), sampler, output views/framebuffers, source view cache keyed by `(image, format, layer)`.
  - Vulkan: if params Some and sample count 1, barrier source to `SHADER_READ_ONLY_OPTIMAL`, render pass to swapchain layer, barrier back.
  - GL: no support, old copy path.
  - Fake: default false, record params.
  - Wire `submit_impl` to input-sized params for PoC.
- [ ] **Step 4: Run** — `cargo test`, `cargo xbuild`.
- [ ] **Step 5: Manual pass-through** — config enabled with strength 0; image unchanged, no resize loop; verify bounds/flip.
- [ ] **Step 6: Commit**

```bash
git add shaders/src/upscaler.vert shaders/src/upscaler.frag shaders/src/lib.rs src/graphics_backends.rs src/graphics_backends/vulkan.rs src/graphics_backends/vulkan/upscaler.rs src/graphics_backends/gl.rs src/compositor.rs
git commit -m "feat(upscaler): add Vulkan pass-through pipeline"
```

---

### Task 4: Fast Path, Output Size, Continuous Weight, A/B

**Files:** `shaders/src/fast.frag`, `src/upscaler/*`, `src/compositor.rs`, `src/graphics_backends/vulkan/upscaler.rs`

- [ ] **Step 1: Failing tests** — `params_uses_recommended_size_and_output_scale`, `exact_swapchain_reuse_requires_same_size`, `weight_field_*`.
- [ ] **Step 2: Run to fail**.
- [ ] **Step 3: Implement**:
  - `UpscalerConfig::params`; `Compositor::Submit` computes base size when enabled; `submit_impl` filters support/sample count, overrides `new_info` size, passes `exact_size`.
  - `fast.frag`: bilinear base; simple adaptive sharpen with `strength * weight_at(uv)`; no zones; `show_region` draws the weight contour at `radius`.
  - `balanced` temporarily maps to Fast until Task 7.
  - Clamp UVs; preserve bounds/flip.
- [ ] **Step 4: Run** — tests + `cargo test`.
- [ ] **Step 5: Manual** — `xrizer_scale=0.7`, fast, center/radius/feather variations, full via `radius=2.0`, A/B, weight contour, bounds/flip.
- [ ] **Step 6: Commit**

```bash
git add shaders/src/fast.frag src/upscaler src/compositor.rs src/graphics_backends/vulkan/upscaler.rs
git commit -m "feat(upscaler): add fast weighted upscaling"
```

---

### Task 5: Alpha Preservation and Safe Fallbacks

**Files:** `shaders/src/fast.frag`, `src/upscaler/*`, `src/compositor.rs`, `src/graphics_backends/vulkan/upscaler.rs`

- [ ] **Step 1: Failing tests** — `preserve_alpha_is_in_flags`, `upscaler_requires_single_sample`.
- [ ] **Step 2: Run to fail**.
- [ ] **Step 3: Implement** — alpha guard before weighted sharpen; skip weight contour on transparent pixels; `upscaler_allowed(supports, sample_count)`; fallback on pipeline/format failure; no extent override when filtered.
- [ ] **Step 4: Run** — tests + `cargo test`.
- [ ] **Step 5: Manual** — transparent VRChat UI, MSAA fallback, OpenGL unchanged.
- [ ] **Step 6: Commit**

```bash
git add shaders/src/fast.frag src/upscaler src/compositor.rs src/graphics_backends/vulkan/upscaler.rs
git commit -m "fix(upscaler): preserve alpha and fall back safely"
```

---

## Phase 1: Classical Baseline (FSR 1 + SGSR 1)

### Task 6: FSR 1 EASU Full-Frame Base + Weighted RCAS

**Files:** `shaders/src/fsr_easu.frag`, `shaders/src/enhance.frag`, `shaders/src/lib.rs`, `src/graphics_backends/vulkan/upscaler.rs`, `README*`

- [ ] **Step 1: Failing test** — `fsr_preset_selects_fsr_kind`; FSR constants finite/non-zero.
- [ ] **Step 2: Run to fail**.
- [ ] **Step 3: Implement**:
  - Add FSR 1 EASU/RCAS GLSL under MIT; preserve AMD notice.
  - Intermediate output-sized textures (array 2) with `COLOR_ATTACHMENT | SAMPLED` (and `STORAGE` for ML).
  - EASU pass: source → intermediate, **full frame**, CPU `FsrEasuCon` constants; sRGB/gamma handling; anti-aliased input assumption documented.
  - `enhance.frag`: weighted RCAS on the EASU intermediate; `rcas_strength = strength * weight_at(uv)`; no zone composite; full radius -> full strength. Preserve alpha (`alpha < 0.999` bypasses RCAS) using the Task 5 guard.
  - `preset=fsr` selects EASU+weighted RCAS; `balanced` still Fast until Task 7.
- [ ] **Step 4: Run** — test + `cargo xbuild`.
- [ ] **Step 5: Manual** — compare fast/fsr at 0.5–0.7; verify the continuous weight gradient (high at center, base only where weight is zero); no gamma shift/ringing.
- [ ] **Step 6: Commit**

```bash
git add shaders/src/fsr_easu.frag shaders/src/enhance.frag shaders/src/lib.rs src/graphics_backends/vulkan/upscaler.rs README.md README.ja.md
git commit -m "feat(upscaler): add FSR1 base and weighted RCAS"
```

---

### Task 7: SGSR 1 as Full-Frame Base Filter

**Files:** `shaders/src/sgsr1.frag`, `src/graphics_backends/vulkan/upscaler.rs`, `src/upscaler/*`, `README*`

- [ ] **Step 1: Failing test** — `balanced_preset_selects_sgsr_kind`; weight tests for SGSR sharpening.
- [ ] **Step 2: Run to fail**.
- [ ] **Step 3: Implement**:
  - Add Snapdragon GSR 1 (SGSR 1) single-pass 12-tap Lanczos-like + adaptive sharpening; BSD-3 notice.
  - `balanced`: SGSR 1 **full-frame** base; its adaptive-sharpening strength is multiplied by `w` (no separate spatial branch). Preserve alpha (`alpha < 0.999` bypasses adaptive sharpening).
  - `fsr`/`ml` remain EASU-base; SGSR 1 is a full-frame base option.
  - No zone blend, no separate spatial branch: the continuous weight is the only spatial control.
- [ ] **Step 4: Run** — tests + `cargo xbuild`.
- [ ] **Step 5: Manual** — SGSR full-frame vs FSR EASU; weight gradient; no double gamma; both eyes identical.
- [ ] **Step 6: Commit**

```bash
git add shaders/src/sgsr1.frag src/graphics_backends/vulkan/upscaler.rs src/upscaler README.md README.ja.md
git commit -m "feat(upscaler): add SGSR1 full-frame base filter"
```

---

## Phase 2: MLP Infrastructure and Inner-Loop Measurement

### Task 8: Model Format + DP4A Per-Pixel MLP Baseline

**Files:** `src/upscaler/model.rs`, `shaders/src/mlp_dp4a.comp`, `shaders/src/lib.rs`, `src/graphics_backends/vulkan/upscaler.rs`, `src/upscaler/*`

**Purpose:** This is **not** the final ML design. It is the first inference infrastructure (model format, loading, switching, measurement) and the measured baseline.

- [ ] **Step 1: Failing tests** — parse `XRU1` MLP with hidden 8/16; reject bad magic/version/truncation; 28 features; CPU reference for one 16→16 layer vs DP4A packing; `ml_bounds` matches weight region.
- [ ] **Step 2: Run to fail**.
- [ ] **Step 3: Implement**:
  - Neutral `XRU1` format (version 1) with `model_kind` (Mlp/ResidualCnn/Offset), feature count, hidden/channels, layer count, input/activation/weight scales, residual clamp, weights i8, biases i32. MLP hidden 8/16.
  - Loader/cache by path+mtime; default `~/.config/xrizer/upscaler_model.xru`; `model_path` override; missing/invalid → ML unavailable.
  - DP4A compute shader: per-pixel 5×5 luma + phase + scale → 28 features; 16 hidden; output residual; clamp; **multiply by `w`**; preserve alpha; write to EASU intermediate. Dispatch only `ml_bounds(epsilon)`; outside remains base.
  - `preset=ml`; with `XRIZER_UPSCALER_VK_LAYER=1`, DP4A should be enabled on VRChat/DXVK. Fallback to `fsr` only if the layer is absent/disabled, the physical device/driver lacks the feature, or the model is missing/invalid.
  - Use a tiny hand-authored/self-test model for smoke; no user weights.
- [ ] **Step 4: Run** — tests + `cargo xbuild`.
- [ ] **Step 5: Manual** — self-test model runs/falls back; weight gradient; no crash; baseline quality/perf recorded.
- [ ] **Step 6: Commit**

```bash
git add src/upscaler/model.rs src/upscaler shaders/src/mlp_dp4a.comp shaders/src/lib.rs src/graphics_backends/vulkan/upscaler.rs
git commit -m "feat(upscaler): add model format and DP4A MLP baseline"
```

---

### Task 9: Training Tool and Methodology (Implement Now, Train Later)

**Files:** `tools/train_upscaler.py`, `tools/eval_temporal.py`, `tools/requirements-upscaler.txt`, `tools/upscaler_dataset.md`, `src/upscaler/model.rs`, `.gitignore`

**Training methodology to encode in the tool and docs:**

- LR generation: **area/box downscale primary**, mixed with the actual render-scale resample (if known) and a small proportion of bicubic/Lanczos; never train only on bicubic/Lanczos; no jitter sequences; random sub-pixel shift augmentation is allowed.
- Capture metadata: game/title, AA mode (TAA/FXAA/MSAA/none), colour space (sRGB/linear/HDR), render scale, eye, scene. FSR 1 requires anti-aliased input; document AA.
- Native LR gap test: capture the **same static viewpoint rendered natively at LR and HR**; use as a test-only set; compare synthetic-downscale vs native-LR quality; fine-tune if the gap is large.
- Scale buckets: train at the exact shipped scales (e.g. 0.67→1.49×, 0.5→2×); avoid random continuous scale for tiny nets.
- Losses: **Charbonnier/L1 primary + gradient (Sobel/Laplacian) + small-weight LPIPS**. **No GAN.** Add a high-frequency gain penalty and residual clamp; under-sharpen slightly and let RCAS finish.
- QAT: fake-quantise weights/activations to the exact INT8 scales used by the shader; export per-channel scales; measure quality after quantisation.
- Temporal evaluation: video sequences; **tLPIPS** (offline optical flow), warping error, IF-SSIM-style, per-pixel temporal variance; residual clamp as the primary flicker mitigation.
- Stereo: both eyes identical deterministic function, same weights, same tile geometry/dispatch; no per-eye normalisation; evaluate left/right difference on mirrored content.
- Budget: GPU timestamp measurement; kill-switch to FSR/SGSR if over budget.

- [ ] **Step 1: Failing tests** — model parser supports MLP/ResidualCnn/Offset; `residual_clamp` validation; dataset metadata parsing; `--self-test`.
- [ ] **Step 2: Run to fail**.
- [ ] **Step 3: Implement**:
  - `train_upscaler.py` supports `--model mlp|residual_cnn|srpo`, `--hidden 8|16`, `--channels`, `--scale`, `--captures`, `--output`, `--self-test`, QAT, export `XRU1`.
  - `eval_temporal.py` reads a processed video sequence and computes tLPIPS/warping/IF-SSIM/temporal variance.
  - `upscaler_dataset.md` documents capture layout, AA/colour metadata, area/box + mixed kernels, native LR test set, privacy/licensing.
  - `.gitignore` covers `captures/`, `*.xru`, generated models.
- [ ] **Step 4: Run** — `cargo test upscaler::model::tests`; `python3 tools/train_upscaler.py --self-test` if PyTorch available (not CI gate).
- [ ] **Step 5: Commit**

```bash
git add tools src/upscaler/model.rs .gitignore
git commit -m "feat(upscaler): add training and temporal evaluation tools"
```

---

### Task 10: DP4A vs Cooperative-Matrix Microbenchmark and Decision

**Files:** `shaders/src/ml_bench.comp`, `src/upscaler/inner_loop.rs`, `src/graphics_backends/vulkan/upscaler.rs`, `docs/upscaler-benchmark.md` (preliminary)

**Decision rule:** DP4A is the default. Cooperative matrix is kept only if the on-target benchmark at realistic **weight-bounding-box** tile counts shows a meaningful win (e.g. ≥15% lower GPU time or materially lower variance) **and** the game's device actually enables it. Otherwise remove the coopmat path from the shipping build or keep it behind an experiment flag.

- [ ] **Step 1: Failing tests** — `choose_inner_loop` selects DP4A when available; coopmat only when DP4A unavailable or benchmark says so; rejects missing features.
- [ ] **Step 2: Run to fail**.
- [ ] **Step 3: Implement**:
  - `ml_bench.comp` with two kernels: 16→16 int8 DP4A and 16×16×16 int8 coopmat, same data/layout.
  - GPU timestamp queries around each at VR weight-bounding-box tile counts and both eyes.
  - Probe `shaderIntegerDotProduct`, `VK_KHR_cooperative_matrix`, `shaderInt8`, storage-buffer-8bit, wave size, coopmat properties.
  - Record time, variance, compile success, fallbacks. Decision documented.
- [ ] **Step 4: Run** — `cargo test choose_inner_loop`, `cargo xbuild`; manual benchmark on RX 7900 XTX **with `XRIZER_UPSCALER_VK_LAYER=1`** and with the layer disabled, to prove the layer is what unlocks DP4A.
- [ ] **Step 5: Commit**

```bash
git add shaders/src/ml_bench.comp src/upscaler/inner_loop.rs src/graphics_backends/vulkan/upscaler.rs docs/upscaler-benchmark.md
git commit -m "test(upscaler): benchmark DP4A against cooperative matrix"
```

---

## Phase 3: Hybrid Residual CNN (Main ML Target)

### Task 11: Hybrid Residual CNN Inference

**Files:** `shaders/src/residual_cnn.comp`, `src/graphics_backends/vulkan/upscaler.rs`, `src/upscaler/model.rs`, `src/upscaler/*`, `README*`

**Design:** EASU/classical base full-frame → tiny INT8 CNN (QuickSRNet/FSRCNN family, 10–30k params) → **clamped residual × weight** → weighted RCAS. No zone split; SGSR 1 is the Balanced full-frame base option, not a spatial branch. QAT weights. This is the main ML bet; it must beat FSR 1 in-headset on quality-per-ms, not on stills alone.

- [ ] **Step 1: Failing tests** — `residual_clamp` bounds; model kind/channels/layers parse; fallback when model incompatible; deterministic same input→same output; `ml_bounds` used.
- [ ] **Step 2: Run to fail**.
- [ ] **Step 3: Implement**:
  - `residual_cnn.comp`: fused LR/HR-friendly CNN with 3×3 convs, 16 channels (parameterized 8/16/32), 2–3 layers, 10–30k params; input = EASU base + local source features; output = RGB residual clamped to e.g. ±0.1–0.2 of local range; multiply by `w`; add to base.
  - Use DP4A by default; with the layer active this is the normal VRChat path. If Task 10 chose coopmat, add a coopmat variant behind the same interface, not a separate architecture.
  - Dispatch only `ml_bounds(epsilon)`; outside remains base.
  - Weighted RCAS after ML (`strength × w`); preserve alpha; both eyes identical tile geometry/weights.
  - Model manifest includes architecture, channels, layers, residual clamp, scale buckets, colour space.
  - Fallback to FSR if model/feature/pipeline fails.
- [ ] **Step 4: Run** — tests + `cargo xbuild`.
- [ ] **Step 5: Manual** — self-test/dummy model smoke; after captures, compare FSR vs residual CNN on stills and video; check weight gradient, shimmer/ghosting, text/UI, stereo difference.
- [ ] **Step 6: Commit**

```bash
git add shaders/src/residual_cnn.comp src/graphics_backends/vulkan/upscaler.rs src/upscaler README.md README.ja.md
git commit -m "feat(upscaler): add hybrid residual CNN inference"
```

---

### Task 12: SRPO-Style Offset/Kernel Prediction (Next Stage)

**Files:** `shaders/src/srpo.comp`, `src/upscaler/model.rs`, `src/graphics_backends/vulkan/upscaler.rs`, `tools/train_upscaler.py`, `src/upscaler/*`

**Design:** After the residual CNN plateaus on aliased/thin geometry, predict **sampling offsets / small kernels** rather than free RGB. Use guided fusion with classical interpolation in flat areas; INT8; apply only where `w > 0`; same full-frame base and weighted RCAS. SRPO-style is the highest quality-per-parameter for rasterised/aliased content but more engineering; it is the next stage, not the first ship.

- [ ] **Step 1: Failing tests** — `model_kind=Offset` parse; offset bounds/clamp; deterministic processing; `ml_bounds`.
- [ ] **Step 2: Run to fail**.
- [ ] **Step 3: Implement**:
  - `srpo.comp`: 3-layer CNN predicts per-pixel offsets/kernel weights; guided fusion with EASU/Lanczos base; clamp offsets; fallback to base in flat/low-confidence areas; multiply residual/offset influence by `w`.
  - Dispatch only `ml_bounds(epsilon)`.
  - Training tool supports `--model srpo`; same LR/loss/QAT methodology; offset regularization.
  - Fallback to residual CNN/FSR if model missing or pipeline fails.
- [ ] **Step 4: Run** — tests + `cargo xbuild`.
- [ ] **Step 5: Manual** — compare residual CNN vs SRPO on aliased/thin geometry; check temporal stability; do not ship if it shimmers more.
- [ ] **Step 6: Commit**

```bash
git add shaders/src/srpo.comp src/upscaler/model.rs src/graphics_backends/vulkan/upscaler.rs tools/train_upscaler.py src/upscaler
git commit -m "feat(upscaler): add SRPO-style offset prediction"
```

---

## Phase 4: Operations, Documentation, and Benchmark

### Task 13: Auto-Scale, Diagnostics, and Documentation

**Files:** `src/upscaler/auto_scale.rs`, `src/upscaler/mod.rs`, `src/settings.rs`, `src/compositor.rs`, `README.md`, `README.ja.md`

- [ ] **Step 1: Failing tests** — auto-scale 30 slow/60 fast frames; no oscillation; scale get/set.
- [ ] **Step 2: Run to fail**.
- [ ] **Step 3: Implement**:
  - `auto_scale`, `min_scale`, `max_scale`; 0.05 steps; 30/60 frame hysteresis; target display period.
  - `settings::{current_supersample_scale,set_supersample_scale}`; clamp/persist.
  - Diagnostics: frames, upscaled, FSR, SGSR, MLP, residual CNN, SRPO, fallbacks, last reason, GPU timestamps. Summary every 300 frames; no image contents.
  - README: config path, presets, `center/radius/feather/strength`, full-frame via `radius=2.0`, recommended `xrizer_scale=0.7`, FSR/SGSR licences, ML model path/training, **Vulkan layer opt-in (`XRIZER_UPSCALER_VK_LAYER=1`, install script / `VK_ADD_IMPLICIT_LAYER_PATH`, Steam launch options, WiVRn environment)**, device feature prerequisites, weighted ML bounds, budget/kill-switch, temporal/stereo limitations.
- [ ] **Step 4: Run** — tests + `cargo xbuild`.
- [ ] **Step 5: Commit**

```bash
git add src/upscaler src/settings.rs src/compositor.rs README.md README.ja.md
git commit -m "feat(upscaler): add auto-scale, diagnostics, and docs"
```

---

### Task 14: Train and Benchmark With User-Provided Captures (Blocked Until Data Arrives)

**Files:** `docs/upscaler-benchmark.md`, `README*` if needed.

- [ ] **Step 1: Wait for captures** — place under `captures/highres/` (gitignored); confirm privacy/usage; do not use invented VRChat content.
- [ ] **Step 2: Train** — MLP baseline, residual CNN (16 and 8 channels), SRPO; export `XRU1`; record QAT metrics and native-LR gap.
- [ ] **Step 3: Evaluation matrix**:
  - Layer: `XRIZER_UPSCALER_VK_LAYER=1` vs disabled; record loader/layer status and whether DP4A/coopmat became available
  - `xrizer_scale`: 1.0, 0.8, 0.7, 0.5
  - Presets: off, fast, balanced (SGSR), fsr, ml (MLP), ml (residual CNN), ml (SRPO)
  - Weight fields: `center` variants, `radius` 0.2/0.35/0.5/2.0, `feather` 0.05/0.1/0.2, `strength` 0.5/1.0/1.5
  - Scenes: text/thin geometry, dark, bright, menu/transparent UI, motion/video
  - Metrics: subjective sharpness, ringing, ghosting, shimmer, stereo difference, frame time/FPS, GPU timestamps, fallback status, ML dispatch bounds/time
  - Temporal: tLPIPS, warping error, IF-SSIM-style, temporal variance on static-micro-motion clips
  - Native LR vs synthetic LR gap
  - If the runtime/headset distortion mesh is available, evaluate the final result **post-warp** as well; otherwise document that evaluation is pre-warp only
- [ ] **Step 4: Final checks** — `cargo test`, `cargo clippy --workspace --all-targets`, `cargo fmt --check`, `cargo xbuild --release`.
- [ ] **Step 5: Write `docs/upscaler-benchmark.md`** — environment (Mesa/RADV, RX 7900 XTX, runtime, HMD, refresh), results, best settings, failures, whether ML beats FSR/SGSR, whether SRPO beats residual CNN, and the kill-switch threshold.
- [ ] **Step 6: Commit**

```bash
git add docs/upscaler-benchmark.md README.md README.ja.md
git commit -m "docs(upscaler): record training and benchmark results"
```

---

## Out of Scope / Follow-Up Plans

1. **FSR 2/3/4** — out of scope; requires motion vectors/depth/history, and FSR 4 is proprietary/FP8-targeted.
2. **Vulkan-layer scope** — the layer only edits `vkCreateDevice` feature/extension enablement. It does not intercept command buffers, queues, images, or game logic, and does not install system-wide without the user's install script. Broader layer features are a follow-up.
3. **Cooperative vector (`VK_NV_cooperative_vector` / cross-vendor EXT)** — not available on AMD Vulkan today; keep the inner-loop interface isolated for future retargeting.
4. **Projection-plane / visual-angle exact weight mapping** — future refinement; baseline uses pre-distortion eye-texture radial distance.
5. **Temporal accumulation / history** — research option only; no engine history/MVs. A compositor-owned pose-only history is a separate high-risk experiment, not v1.
6. **Eye tracking/OSC** — excluded by the user.
7. **OpenGL parity** — only if native OpenGL VR titles matter.

## Open Questions / Confirmation

1. **Layer opt-in and install path:** the plan uses `XRIZER_UPSCALER_VK_LAYER=1` (disable `XRIZER_UPSCALER_VK_LAYER_DISABLE=1`), with either `VK_ADD_IMPLICIT_LAYER_PATH=<runtime>/vulkan/implicit_layer.d` or `scripts/install-vulkan-layer.sh --user` into `$XDG_DATA_HOME/vulkan/implicit_layer.d`. Is this acceptable, and should the plan prefer the user-install (recommended for Steam Linux Runtime/pressure-vessel) or the portable env path?
2. **Layer feature scope:** DP4A is the primary feature; coopmat is enabled by the same layer for the microbenchmark. If the layer cannot safely edit the pNext chain, it passes through and ML falls back. Is that fail-open behavior sufficient?
3. **Model filename:** default `~/.config/xrizer/upscaler_model.xru`, with `model_path` override in `upscaler.json`. Is this acceptable?
4. **Training timing:** training/evaluation tools are implemented now; actual training and benchmark (Task 14) wait for user-provided captures. Is that the intended split?
5. **SRPO scope:** SRPO is the stage after the residual CNN; the first shippable ML preset is the residual CNN. Is that ordering correct, or should SRPO be required before the first release?

## Self-Review

- **Spec coverage:** opt-in Vulkan implicit layer and fail-open feature injection (Tasks L1/L2), continuous weight field (Tasks 1, 4, 6, 7, 8, 11, 12), full-frame base (Tasks 6, 7), Fast (Task 4), SGSR 1 as full-frame base (Task 7), FSR 1 base (Task 6), MLP infrastructure (Task 8), DP4A/coopmat benchmark (Task 10), hybrid residual CNN main target (Task 11), SRPO next stage (Task 12), training methodology (Task 9), temporal/stereo (Tasks 9, 13, 14), output size/resolution (Tasks 2, 4), alpha (Task 5), config `upscaler.json` (Task 1), device probe/fallback (Tasks 8, 10, 11).
- **Step scan:** each Rust task has a failing test, implementation, verification, commit. Shader-only tasks use `cargo xbuild` plus manual HMD checks because GPU output is not unit-testable.
- **Type consistency:** `UpscalerConfig`, `UpscalerParams`, `UpscalerPreset`, `center/radius/feather/strength`, `dst_width/dst_height`, `src_bounds`, `weight_at`, `ml_bounds`, `with_fallback`, `supports_upscaler`, `supports_ml_upscaler`, `choose_inner_loop`, `residual_clamp` are used consistently.
- **Review focus:** layer fail-open/device capability (Tasks L1/L2/8/10/11), continuous weight field (Task 1/4/6/7), temporal/stereo (Tasks 9/13/14), bounds/alpha/gamma (Tasks 1/5/6/7), resolution/model fallback (Tasks 4/8/11) each have a test or manual check.
- **Proportion:** the plan fixes interfaces, formats, tests, and gates; it does not transcribe the full FSR/SGSR/CNN/SRPO shader bodies.
