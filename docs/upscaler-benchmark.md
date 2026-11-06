# Upscaler benchmark — preliminary

> **Historical:** this benchmark was written for the pre-B1a spatial
> upscaler. The files `src/upscaler/inner_loop.rs`, `src/upscaler/model.rs`,
> and the ML/FSR/SGSR shaders were removed in B1a. The current AA-only
> benchmark should measure the equal-size FXAA/CMAA2/SMAA passes instead.

Status: **preliminary**. The decision logic and benchmark shaders are in the
tree; on-target timing on the RX 7900 XTX / RADV / WiVRn stack is still required.
No numbers are recorded here until they are measured on that machine.

## Decision rule

- DP4A (`VK_KHR_shader_integer_dot_product`) is the default inner loop.
- Cooperative matrix (`VK_KHR_cooperative_matrix`, RDNA 3 WMMA 16x16x16) is
  kept only when:
  1. `cooperativeMatrix`, `shaderInt8`, `storageBuffer8BitAccess`, and
     `vulkanMemoryModel` are all enabled, and
  2. the on-target benchmark shows at least a 15% lower GPU time than DP4A at
     realistic weight-bounding-box tile counts, or materially lower variance.
- If DP4A is unavailable and cooperative matrix is fully available, cooperative
  matrix is the fallback.
- If neither is available, ML is unavailable and `preset=ml` falls back to FSR 1.

The pure decision is implemented in `src/upscaler/inner_loop.rs` and tested by
`choose_inner_loop_*` tests.

## Harness

- `shaders/src/ml_bench.comp`: 16-wide int8 DP4A kernel.
- `shaders/src/ml_bench_coopmat.comp`: 16x16x16 int8 cooperative-matrix kernel.
- `vulkan-layer-probe` creates both pipelines through the opt-in layer and
  reports `dp4a=ok/fallback` and `coopmat=ok/fallback`.

Run with the layer:

```sh
cargo build -p xrizer-vulkan-layer
cargo xbuild
XRIZER_UPSCALER_VK_LAYER=1 \
VK_ADD_IMPLICIT_LAYER_PATH="$PWD/target/debug/vulkan/implicit_layer.d" \
  cargo run -p xrizer-vk-probe
```

Run without the layer for the control case. `VK_LOADER_DEBUG=layer` confirms
whether `VK_LAYER_XRIZER_upscaler` was inserted.

## To record on target

| Field | Value |
|---|---|
| GPU / driver / Mesa | RX 7900 XTX, RADV, Mesa version |
| Runtime / HMD / refresh | WiVRn or Monado, HMD, 72/90/120 Hz |
| xrizer_scale | 1.0, 0.8, 0.7, 0.5 |
| Weight field | radius 0.2 / 0.35 / 0.5 / 2.0, feather 0.05 / 0.1 / 0.2 |
| Tiles | both eyes, ML bounds at each weight field |
| DP4A time / variance | GPU timestamps, multiple runs |
| Coopmat time / variance | GPU timestamps, multiple runs |
| Compile / fallback status | pipeline creation and layer log |
| Decision | DP4A or cooperative matrix, with the measured reason |

Do not enable cooperative matrix in the shipping path until this table has real
measurements and the 15% rule is met.
