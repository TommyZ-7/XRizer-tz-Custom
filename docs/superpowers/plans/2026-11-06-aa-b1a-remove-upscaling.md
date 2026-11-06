# XRizer AA-Only B1a Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the spatial upscaler's presets, FSR/EASU, SGSR, ML/model paths, and output scaling, leaving an equal-size post-process pipeline that B1b will populate with FXAA.

**Architecture:** The config module keeps the `upscaler.json` file name but exposes AA-only settings: method, weight mode, weight field, compare, and the retained render-scale controls. The Vulkan pipeline becomes a minimal equal-size copy pipeline; B1b adds the AA shader. No upscaling path remains.

**Tech Stack:** Rust 2024, ash/Vulkan, GLSL 450, OpenXR swapchain, serde_json.

**Spec:** `../../../../xrizer-aa-b1a/docs/contracts/settings-v1.md` (the Phase 0 contract in the config worktree) and `tz-custom-suite-plan.md`.

## Global Constraints

- The input and output extent are identical. There is no `output_scale`.
- `auto_scale`, `min_scale`, `max_scale`, and `xrizer_scale` remain; they are game render-scale controls.
- `weight_mode` is `center`, `inverted`, or `whole`; default `inverted`.
- `antialias` is `off`, `cmaa2`, `smaa`, or `fxaa`; default `cmaa2` in the final contract. B1a does not render AA yet.
- Deprecated keys are ignored and preserved by the config app; the runtime never reads them.
- The capture metadata stays readable; `output_scale` is always `1.0`.
- Run `cargo fmt --check`, `cargo clippy --workspace --all-targets`, `cargo test`, and `cargo xbuild`.

## Review Focus

- Equal-size output: a configured AA pass must never resize the swapchain.
- Weight modes: `whole` must not read stale `radius`/`feather` values.
- Legacy keys: `preset`, `output_scale`, and `model_path` must parse without error and be ignored.
- Compare: the A/B interval must come from `compare_interval_ms`, not a hard-coded 2 seconds.
- Capture: metadata must remain valid JSON and report `output_scale: 1.0`.

---

### Task 1: AA config schema and weight math

**Files:**
- Modify: `src/upscaler/mod.rs`
- Modify: `src/upscaler/math.rs`
- Test: `src/upscaler/mod.rs` (`mod tests`)

**Interfaces:**
- Produces: `enum AntialiasMethod { Off, Cmaa2, Smaa, Fxaa }`
- Produces: `enum WeightMode { Center, Inverted, Whole }`
- Produces: `enum FxaaQuality { Performance, Balanced, Quality }`
- Produces: `UpscalerConfig` fields `antialias`, `weight_mode`, `center_offset`, `fxaa_quality`, `smaa_threshold`, `compare_interval_ms`.
- Produces: `UpscalerParams::center_for_eye(eye: usize) -> [f32; 2]` and `weight_at_for_eye(uv, eye) -> f32`.
- Produces: `math::weight_at_mode(uv, center, radius, feather, aspect, mode) -> f32`.

- [ ] **Step 1: Write the failing config tests**

Add to `src/upscaler/mod.rs` tests:

```rust
#[test]
fn default_config_is_aa_only() {
    let c = UpscalerConfig::default();
    assert!(!c.enabled);
    assert_eq!(c.antialias, AntialiasMethod::Cmaa2);
    assert_eq!(c.weight_mode, WeightMode::Inverted);
    assert_eq!(c.center, [0.5, 0.5]);
    assert_eq!(c.center_offset, 0.0);
    assert_eq!(c.radius, 0.35);
    assert_eq!(c.feather, 0.10);
    assert_eq!(c.strength, 1.0);
    assert!(c.preserve_alpha);
    assert_eq!(c.fxaa_quality, FxaaQuality::Quality);
    assert_eq!(c.smaa_threshold, 0.1);
    assert!(!c.compare);
    assert_eq!(c.compare_interval_ms, 2000);
    assert!(!c.show_region);
    assert!(!c.auto_scale);
    assert_eq!(c.min_scale, 0.5);
    assert_eq!(c.max_scale, 1.5);
}

#[test]
fn params_are_always_equal_size() {
    let c = UpscalerConfig {
        enabled: true,
        ..Default::default()
    };
    let p = c.params_for_size(1000, 800, full_bounds()).unwrap();
    assert_eq!((p.dst_width, p.dst_height), (1000, 800));
}

#[test]
fn legacy_upscaling_keys_are_ignored() {
    let c: UpscalerConfig = serde_json::from_str(
        r#"{"enabled":true,"preset":"ml","output_scale":0.5,"model_path":"/tmp/x"}"#,
    )
    .unwrap();
    assert!(c.enabled);
    assert_eq!(c.antialias, AntialiasMethod::Cmaa2);
}

#[test]
fn parse_all_aa_methods_and_weight_modes() {
    for (method, expected) in [
        ("off", AntialiasMethod::Off),
        ("cmaa2", AntialiasMethod::Cmaa2),
        ("smaa", AntialiasMethod::Smaa),
        ("fxaa", AntialiasMethod::Fxaa),
    ] {
        let text = format!(r#"{{"antialias":"{method}"}}"#);
        let c: UpscalerConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(c.antialias, expected);
    }
    for (mode, expected) in [
        ("center", WeightMode::Center),
        ("inverted", WeightMode::Inverted),
        ("whole", WeightMode::Whole),
    ] {
        let text = format!(r#"{{"weight_mode":"{mode}"}}"#);
        let c: UpscalerConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(c.weight_mode, expected);
    }
}

#[test]
fn compare_phase_uses_the_configured_interval() {
    assert!(compare_phase(0, 2000));
    assert!(compare_phase(1999, 2000));
    assert!(!compare_phase(2000, 2000));
    assert!(!compare_phase(3999, 2000));
    assert!(compare_phase(4000, 2000));
}
```

- [ ] **Step 2: Write the failing weight-mode tests**

Add to `src/upscaler/math.rs` tests:

```rust
#[test]
fn center_mode_matches_the_existing_field() {
    let center = [0.5, 0.5];
    assert_eq!(weight_at_mode([0.5, 0.5], center, 0.2, 0.1, 1.0, WeightMode::Center), 1.0);
    assert_eq!(weight_at_mode([0.9, 0.5], center, 0.2, 0.1, 1.0, WeightMode::Center), 0.0);
}

#[test]
fn inverted_mode_is_zero_in_the_centre_and_one_outside() {
    let center = [0.5, 0.5];
    assert_eq!(weight_at_mode([0.5, 0.5], center, 0.2, 0.1, 1.0, WeightMode::Inverted), 0.0);
    assert_eq!(weight_at_mode([0.9, 0.5], center, 0.2, 0.1, 1.0, WeightMode::Inverted), 1.0);
}

#[test]
fn whole_mode_is_one_everywhere() {
    let center = [0.5, 0.5];
    for uv in [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]] {
        assert_eq!(weight_at_mode(uv, center, 0.2, 0.1, 1.0, WeightMode::Whole), 1.0);
    }
}
```

- [ ] **Step 3: Run the tests and watch them fail**

Run: `cargo test --lib upscaler`
Expected: compile errors because the new enums, fields, `compare_phase`, and `weight_at_mode` do not exist.

- [ ] **Step 4: Implement the AA-only config**

In `src/upscaler/mod.rs`:

- Add the three enums with `#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]` and `#[serde(rename_all = "lowercase")]`; `AntialiasMethod` default `Cmaa2`, `WeightMode` default `Inverted`, `FxaaQuality` default `Quality`.
- Replace `UpscalerConfig` fields with the 17 contract keys. Remove `preset`, `output_scale`, and `model_path`.
- Implement `Default` with the contract defaults.
- `sanitized()` clamps `center` to 0..1, `center_offset` to -0.2..0.2, `radius` 0..1, `feather` 0..1, `strength` 0..1, `smaa_threshold` 0.05..0.2, `compare_interval_ms` 500..10000, and `min_scale`/`max_scale` 0.1..4.0 with swap.
- `params_for_size_at` returns equal-size `UpscalerParams`; remove `output_scale` from the calculation.
- Add `center_for_eye(eye)` with left `-center_offset`, right `+center_offset`, clamped to 0..1.
- Add `weight_at_for_eye(uv, eye)` calling `math::weight_at_mode` with the eye center. `whole` returns 1.0 when enabled.
- Remove `UpscalerPreset`, `shader_kind`, `with_fallback`, `fsr_easu_con`, and `ml_bounds`.
- Add the pure `compare_phase(elapsed_ms, interval_ms)` helper: `(elapsed_ms / interval_ms) % 2 == 0`, with interval 0 treated as always on.
- `compare_enabled()` uses the config interval.
- `UpscalerParams` fields: `dst_width`, `dst_height`, `center`, `center_offset`, `radius`, `feather`, `strength`, `src_bounds`, `antialias`, `weight_mode`, `preserve_alpha`, `show_region`, `enabled`. Remove `preset`.
- `flags()` keeps `FLAG_PRESERVE_ALPHA`, `FLAG_SHOW_REGION`, `FLAG_ENHANCEMENT_ENABLED`; remove ML/output-unorm flags.
- `upscaler_allowed` remains the backend/sample-count gate.

- [ ] **Step 5: Implement the three weight modes**

In `src/upscaler/math.rs`:

- Keep `smoothstep` and `weight_at`.
- Add `weight_at_mode(uv, center, radius, feather, aspect, mode)`:
  - `Center`: existing `weight_at`.
  - `Inverted`: `if feather <= 0.0 { if distance <= radius { 0.0 } else { 1.0 } } else { smoothstep(radius, radius + feather, distance) }`.
  - `Whole`: `1.0`.
- Delete `MLP_FEATURE_COUNT`, `extract_luma_features`, `residual_clamp`, `local_residual_clamp`, `fsr_easu_con`, and `ml_bounds` and their tests.

- [ ] **Step 6: Run the tests and watch them pass**

Run: `cargo test --lib upscaler`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/upscaler/mod.rs src/upscaler/math.rs
git commit -m "refactor(aa): make config AA-only"
```

---

### Task 2: Delete ML, model, inner-loop, and scaling shaders

**Files:**
- Delete: `src/upscaler/model.rs`
- Delete: `src/upscaler/inner_loop.rs`
- Modify: `src/upscaler/mod.rs`
- Delete: `shaders/src/fast.frag`, `shaders/src/fsr_easu.frag`, `shaders/src/enhance.frag`, `shaders/src/sgsr1.frag`, `shaders/src/SGSR-LICENSE.txt`
- Delete: `shaders/src/mlp_dp4a.comp`, `shaders/src/ml_bench.comp`, `shaders/src/ml_bench_coopmat.comp`, `shaders/src/residual_cnn.comp`, `shaders/src/srpo.comp`, `shaders/src/dp4a_probe.comp`, `shaders/src/ffx_a.h`, `shaders/src/ffx_fsr1.h`
- Modify: `shaders/src/lib.rs`

- [ ] **Step 1: Remove the modules and shader list entries**

Remove `pub mod inner_loop;` and `pub mod model;`. Remove those shader entries from `shaders/src/lib.rs`; keep `upscaler.vert`, `upscaler.frag`, `overlay.*`, and `capture.*`.

- [ ] **Step 2: Run the build and watch it fail**

Run: `cargo test --lib`
Expected: compile errors in `graphics_backends/vulkan/upscaler.rs`, `graphics_backends/vulkan.rs`, `capture.rs`, `diagnostics.rs`, and `compositor.rs` because the deleted modules and fields are still referenced.

- [ ] **Step 3: Commit the deletions**

```bash
git add -A src/upscaler shaders/src
git commit -m "refactor(aa): remove ML and scaling code"
```

---

### Task 3: Replace the Vulkan pipeline with an equal-size pass

**Files:**
- Rewrite: `src/graphics_backends/vulkan/upscaler.rs`
- Modify: `src/graphics_backends/vulkan.rs`
- Modify: `src/graphics_backends.rs`
- Modify: `src/graphics_backends/gl.rs`

**Interfaces:**
- Produces: `UpscalerPipeline::new(device, target_format, images, width, height) -> Result<Self, vk::Result>`.
- Produces: `UpscalerPipeline::record(device, command_buffer, image_index, eye, source_image, source_format, source_layer, source_extent, params) -> Result<(), vk::Result>`.
- Consumes: `UpscalerParams` from Task 1.

- [ ] **Step 1: Rewrite the pipeline**

Keep only:
- one render pass for `target_format`
- one descriptor set layout for a combined image sampler
- one descriptor pool with two sets
- one pipeline layout with the existing `PushConstants` layout
- one graphics pipeline using `vert_upscaler.spv` and `frag_upscaler.spv`
- sampler, output views, framebuffers, `SourceViewCache`, `ExtentTracker`, width/height
- `prepare_source_views`, `clear_source_views`, `destroy`

Delete intermediate images, EASU/RCAS/SGSR pipelines, ML resources, `create_buffer`, `create_compute_pipeline`, `create_image`, `format_is_srgb`, `probe_dp4a`, `MlPushConstants`, `MlResources`, and all preset branches. `record` always draws one full-screen triangle into the output framebuffer at `self.width/self.height`, with the source descriptor bound.

- [ ] **Step 2: Rewire the backend**

In `vulkan.rs`:
- Remove `ml_feature_supported`, `ml_model_stamp`, `ml_model_attempted`, `supports_ml_upscaler`, and ML model construction.
- Construct `UpscalerPipeline::new(&self.device, data.format, &data.images, data.width, data.height)`.
- Keep the existing `upscaler_failed` fallback and the per-frame `record` call.

In `graphics_backends.rs`, remove `supports_ml_upscaler`. In `gl.rs`, keep the ignored `params` signature.

- [ ] **Step 3: Build**

Run: `cargo xbuild`
Expected: compiles, including the remaining shaders.

- [ ] **Step 4: Commit**

```bash
git add src/graphics_backends src/graphics_backends.rs shaders
git commit -m "refactor(aa): use an equal-size Vulkan pass"
```

---

### Task 4: Rewire compositor, capture, diagnostics, and tests

**Files:**
- Modify: `src/compositor.rs`
- Modify: `src/graphics_backends/vulkan/capture.rs`
- Modify: `src/upscaler/diagnostics.rs`
- Test: `src/compositor.rs` tests

- [ ] **Step 1: Remove preset and output-scale usage**

- In `compositor.rs`, remove the `with_fallback` call and the ML capability branch.
- `params.dst_width/height` are always the input size, so the swapchain is never resized by AA. Remove the "upscaler sized" exact-size special case only if it is no longer needed; keep the normal exact-size behavior for other paths.
- Change `diagnostics::record_frame(params.preset, params.enabled)` to `record_frame(params.antialias, params.enabled)`.
- In `capture.rs`, set `output_scale: 1.0` and `preset: format!("{:?}", snapshot().antialias).to_lowercase()`.
- Simplify diagnostics to count frames and enabled frames by AA method; remove ML counters.

- [ ] **Step 2: Update the compositor tests**

Replace the old output-scale tests with:
- `aa_params_are_equal_size`
- `aa_params_are_opt_in`
- `per_eye_bounds_are_not_shared`
- `compare_uses_configured_interval` (pure helper, if not already covered)
Remove the ML fallback test. Keep the swapchain reuse test if it does not depend on output scaling.

- [ ] **Step 3: Run the full suite**

Run: `cargo fmt --check`
Run: `cargo clippy --workspace --all-targets`
Run: `cargo test`
Run: `cargo xbuild`

Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add src
git commit -m "refactor(aa): remove upscaling from the submit path"
```

---

### Task 5: Documentation and handoff

**Files:**
- Modify: `docs/upscaler-settings.ja.md`
- Modify: `README.ja.md`
- Modify: `README.md`

- [ ] **Step 1: Rewrite the settings guide for AA-only**

Document the 17 AA keys, the three weight modes, the equal-size guarantee, the deprecated keys, and the fact that AA is not implemented until B1b. Remove preset/FSR/SGSR/ML sections.

- [ ] **Step 2: Run the final checks**

Run: `cargo fmt --check`
Run: `cargo clippy --workspace --all-targets`
Run: `cargo test`
Run: `cargo xbuild`

- [ ] **Step 3: Commit and push**

```bash
git add README.md README.ja.md docs/upscaler-settings.ja.md
git commit -m "docs(aa): describe the AA-only configuration"
git push -u origin feat/aa-b1a-remove-upscaling
gh pr create --draft --title "refactor(aa): remove upscaling scaffolding" --body "B1a: equal-size AA pipeline skeleton; FXAA follows in B1b."
```
