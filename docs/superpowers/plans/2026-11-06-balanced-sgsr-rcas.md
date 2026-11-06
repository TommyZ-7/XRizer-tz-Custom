# Balanced SGSR + RCAS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a weighted RCAS post-sharpen pass after SGSR for the `balanced` preset, keeping `compare` as enhancement on/off and `show_region` as the final-pass debug overlay.

**Architecture:** SGSR renders into the existing `R8G8B8A8_SRGB` per-eye intermediate with `FLAG_OUTPUT_UNORM` cleared; the existing RCAS pipeline then sharpens that intermediate into the OpenXR swapchain. Flipped bounds still fall back to `fast`. The SGSR shader's contour block is removed so the final RCAS pass owns `show_region`.

**Tech Stack:** Rust, ash/Vulkan, GLSL 450, OpenXR swapchain.

**Spec:** `xrizer-upscaler-preset-investigation.md` 追記 G (approved 2026-11-06). User decisions: compare semantics unchanged; RCAS addition approved.

## Global Constraints

- `compare` stays "enhancement on/off": base SGSR always runs; RCAS is gated by `FLAG_ENHANCEMENT_ENABLED`.
- `show_region` is drawn by the final pass only; it follows `compare` like `fast`/`fsr`.
- SGSR intermediate is `R8G8B8A8_SRGB`; clear `FLAG_OUTPUT_UNORM` for that pass to avoid double gamma.
- RCAS output keeps `FLAG_OUTPUT_UNORM` when the swapchain is not sRGB.
- `balanced` + flipped bounds still uses `fast`.
- Preserve alpha: `alpha < 0.999` bypasses sharpening when `preserve_alpha=true`.
- Reuse the existing intermediate image, framebuffers, descriptor sets, and RCAS pipeline; no new images or descriptor sets.
- Run `cargo test`, `cargo fmt --check`, `cargo clippy --workspace --all-targets`, `cargo xbuild`.

## Review Focus

- Double gamma: SGSR must not manually sqrt-encode into the sRGB intermediate.
- Contour: must be drawn exactly once, by the final RCAS pass.
- Compare off: RCAS must pass the SGSR intermediate through unchanged.
- Flipped bounds: must still use `fast`, not SGSR+RCAS.
- Alpha `< 0.999`: must bypass SGSR/RCAS and preserve the original pixel.
- Render pass/format mismatch: SGSR pipeline must be created against the intermediate render pass.
- Intermediate layout: barrier between SGSR write and RCAS sample must match the FSR path.

---

### Task 1: Testable balanced routing

**Files:**
- Modify: `src/graphics_backends/vulkan/upscaler.rs`
- Test: same file, existing `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `enum BalancedRoute { SgsrThenRcas, Fast }`; `fn balanced_route(has_flipped_bounds: bool) -> BalancedRoute`.
- Consumes: `UpscalerParams::has_flipped_bounds`.

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
#[test]
fn balanced_route_uses_sgsr_then_rcas_unless_flipped() {
    assert_eq!(balanced_route(false), BalancedRoute::SgsrThenRcas);
    assert_eq!(balanced_route(true), BalancedRoute::Fast);
}
```

- [ ] **Step 2: Run the test and watch it fail**

Run: `cargo test --lib balanced_route_uses_sgsr_then_rcas_unless_flipped`

Expected: compile error because `balanced_route` / `BalancedRoute` do not exist.

- [ ] **Step 3: Implement the helper and use it in the existing pipeline match**

Add near the top of the module:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BalancedRoute {
    SgsrThenRcas,
    Fast,
}

fn balanced_route(has_flipped_bounds: bool) -> BalancedRoute {
    if has_flipped_bounds {
        BalancedRoute::Fast
    } else {
        BalancedRoute::SgsrThenRcas
    }
}
```

In `record`, replace the `Balanced` match arm with:

```rust
UpscalerPreset::Balanced => match balanced_route(params.has_flipped_bounds()) {
    BalancedRoute::SgsrThenRcas => self.sgsr_pipeline,
    BalancedRoute::Fast => self.fast_pipeline,
},
```

This is behavior-preserving at this task boundary.

- [ ] **Step 4: Run the test and the full test suite**

Run: `cargo test --lib balanced_route_uses_sgsr_then_rcas_unless_flipped`
Expected: PASS.

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/graphics_backends/vulkan/upscaler.rs
git commit -m "refactor(upscaler): make balanced routing explicit"
```

---

### Task 2: SGSR intermediate + RCAS output pass

**Files:**
- Modify: `src/graphics_backends/vulkan/upscaler.rs`
- Modify: `shaders/src/sgsr1.frag`
- Test: `src/graphics_backends/vulkan/upscaler.rs` (`mod tests`)

**Interfaces:**
- Produces:
  - `fn sgsr_intermediate_flags(flags: u32) -> u32` (clears `FLAG_OUTPUT_UNORM`).
  - `unsafe fn record_sgsr_intermediate(&self, device, command_buffer, eye, push: &PushConstants)`.
  - `unsafe fn barrier_intermediate_read(&self, device, command_buffer, eye)`.
  - `unsafe fn record_rcas_output(&self, device, command_buffer, image_index, eye, push: &PushConstants, rcas_set: vk::DescriptorSet)`.
- Consumes: existing `easu_render_pass`, `intermediate_framebuffers`, `intermediate_image`, `rcas_pipeline`, `rcas_layout`, `rcas_fsr_sets`.

- [ ] **Step 1: Write the failing flag test**

Add to `mod tests`:

```rust
use crate::upscaler::{FLAG_ENHANCEMENT_ENABLED, FLAG_PRESERVE_ALPHA, FLAG_SHOW_REGION};

#[test]
fn sgsr_intermediate_flags_clear_output_unorm() {
    let flags = FLAG_PRESERVE_ALPHA
        | FLAG_SHOW_REGION
        | FLAG_ENHANCEMENT_ENABLED
        | FLAG_OUTPUT_UNORM;
    let intermediate = sgsr_intermediate_flags(flags);
    assert_eq!(intermediate & FLAG_OUTPUT_UNORM, 0);
    assert_eq!(
        intermediate,
        FLAG_PRESERVE_ALPHA | FLAG_SHOW_REGION | FLAG_ENHANCEMENT_ENABLED
    );
}
```

- [ ] **Step 2: Run the test and watch it fail**

Run: `cargo test --lib sgsr_intermediate_flags_clear_output_unorm`

Expected: compile error because `sgsr_intermediate_flags` does not exist.

- [ ] **Step 3: Implement the flag helper**

Add:

```rust
fn sgsr_intermediate_flags(flags: u32) -> u32 {
    flags & !FLAG_OUTPUT_UNORM
}
```

- [ ] **Step 4: Run the test and watch it pass**

Run: `cargo test --lib sgsr_intermediate_flags_clear_output_unorm`
Expected: PASS.

- [ ] **Step 5: Create the SGSR pipeline against the intermediate render pass**

Change the SGSR pipeline creation from `render_pass` to `easu_render_pass`:

```rust
let sgsr_pipeline =
    create_graphics_pipeline(device, easu_render_pass, layout, vertex_module, sgsr_module)?;
```

- [ ] **Step 6: Add the three rendering helpers**

Add `record_sgsr_intermediate` (begin `easu_render_pass` on `intermediate_framebuffers[eye]`, bind `sgsr_pipeline`, source descriptor, push constants, draw), `barrier_intermediate_read` (same barrier as the FSR path: `COLOR_ATTACHMENT_OUTPUT` -> `FRAGMENT_SHADER | COMPUTE_SHADER`, `COLOR_ATTACHMENT_WRITE` -> `SHADER_READ`, `SHADER_READ_ONLY_OPTIMAL` -> `SHADER_READ_ONLY_OPTIMAL`, layer `eye`), and `record_rcas_output` (begin `render_pass` on `framebuffers[image_index][eye]`, bind `rcas_pipeline`, descriptor set `rcas_set`, push constants, draw). Reuse `self.width/height` for the render area and viewport.

- [ ] **Step 7: Route balanced through SGSR -> RCAS**

Before the direct-to-swapchain `else` branch, add:

```rust
} else if params.preset == UpscalerPreset::Balanced
    && balanced_route(params.has_flipped_bounds()) == BalancedRoute::SgsrThenRcas
{
    let mut sgsr_push = push;
    sgsr_push.flags = sgsr_intermediate_flags(push.flags);
    self.record_sgsr_intermediate(device, command_buffer, eye, &sgsr_push);
    self.barrier_intermediate_read(device, command_buffer, eye);
    self.record_rcas_output(
        device,
        command_buffer,
        image_index,
        eye,
        &push,
        self.rcas_fsr_sets[eye],
    );
}
```

The direct `match` then only handles `Fast`, flipped `Balanced` -> `fast`, and `Fsr | Ml` -> passthrough. Derive `Clone, Copy` on `PushConstants` so `push` can be copied for the SGSR push constants and still used for RCAS.

- [ ] **Step 8: Replace the FSR inline barrier/RCAS block with the helpers**

Keep the EASU pass and ML dispatch; replace the inline intermediate barrier with `barrier_intermediate_read(...)`, and the inline RCAS render pass with `record_rcas_output(..., &push, rcas_set)`.

- [ ] **Step 9: Remove the SGSR contour block**

Delete `sgsr1.frag` lines 135-140 (the `if ((pc.flags & 2u) != 0u)` block). The final RCAS pass draws the contour.

- [ ] **Step 10: Compile shaders and run tests**

Run: `cargo xbuild`
Expected: builds and compiles `sgsr1.frag` / `enhance.frag`.

Run: `cargo test`
Expected: all tests pass.

- [ ] **Step 11: Commit**

```bash
git add src/graphics_backends/vulkan/upscaler.rs shaders/src/sgsr1.frag
git commit -m "feat(upscaler): add weighted RCAS after SGSR"
```

---

### Task 3: Documentation and final verification

**Files:**
- Modify: `README.md`, `README.ja.md`, `docs/upscaler-settings.ja.md`

- [ ] **Step 1: Update the English README**

In the `balanced` paragraph, state that SGSR 1 is followed by the weighted RCAS pass and that both SGSR adaptive sharpening and RCAS are scaled by the radial weight.

- [ ] **Step 2: Update the Japanese README**

Mirror the same change in the `balanced` paragraph.

- [ ] **Step 3: Update the settings guide**

In `docs/upscaler-settings.ja.md`, change the `balanced` row to "SGSR 1（全面） | 重み付き適応シャープニング + 後段 RCAS". Add a bullet that balanced is a two-pass SGSR -> RCAS path, that compare off returns the SGSR intermediate unchanged, and that `show_region` follows the final enhancement pass (so it is not drawn in the compare off phase).

- [ ] **Step 4: Run all repository checks**

Run: `cargo fmt --check`
Run: `cargo clippy --workspace --all-targets`
Run: `cargo test`
Run: `cargo xbuild`

Expected: all clean.

- [ ] **Step 5: Commit**

```bash
git add README.md README.ja.md docs/upscaler-settings.ja.md
git commit -m "docs(upscaler): document balanced SGSR+RCAS path"
```
