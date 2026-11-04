# Upscaler capture and training dataset

This directory is for **user-provided** captures. Captures, derived frames, and
trained `*.xru` models are git-ignored. Do not commit VRChat content, avatar
names, user IDs, headset serials, IP addresses, or absolute paths.

## Layout

```text
captures/
  highres/
    <game>/
      <scene>/
        metadata.json
        frame_000001.png
        frame_000002.png
        ...
  native_lr/                  # optional, test-only native low-res pairs
    <game>/
      <scene>/
        metadata.json
        lr_000001.png
        hr_000001.png
  eval/
    <sequence>/
      frame_000001.png
      ...
```

`metadata.json` (per scene) must contain:

```json
{
  "game": "VRChat",
  "aa_mode": "taa",
  "color_space": "srgb",
  "render_scale": 0.7,
  "eye": "both",
  "scene": "text-and-thin-geometry",
  "notes": "optional"
}
```

- `aa_mode`: `none`, `fxaa`, `taa`, or `msaa`. FSR 1 expects anti-aliased input.
- `color_space`: `srgb`, `linear`, or `hdr`. Train/evaluate each separately.
- `render_scale`: the linear render scale used for the runtime frame.
- `eye`: `left`, `right`, or `both`. Keep both eyes for stereo checks.

## LR generation

The degradation is part of the model. Use a mixture, weighted toward the
runtime's actual resampling:

1. **Area/box downscale (primary).** This best models averaging real HR samples.
2. **The actual render-scale resample** when it is known.
3. **A small proportion of bicubic/Lanczos** to avoid over-fitting one kernel.

Never train only on bicubic/Lanczos LR. Do not synthesize cross-frame jitter
sequences; the runtime has no history. Random sub-pixel shift augmentation on
single frames is allowed and recommended at fixed scale factors.

Train at the exact shipped buckets (for example `0.67 -> 1.49x` and
`0.5 -> 2x`). Tiny networks generalise worse under continuous random scale.

## Native LR gap test

For a handful of static scenes, capture the same viewpoint rendered natively at
LR and HR. Keep these pairs in `captures/native_lr/` as a **test-only** set.
Compare synthetic-downscale and native-LR quality. If the gap is large,
fine-tune with downscales that mimic native-LR aliasing (weaker prefilter).

## Losses and QAT

- Charbonnier/L1 primary on RGB, with a luma-heavy/chroma-light option.
- Sobel/Laplacian gradient loss.
- Small-weight LPIPS (`<= 0.05`); **no GAN loss**.
- Penalise high-frequency gain above the HR reference; clamp the residual.
- Slightly under-sharpen and let weighted RCAS finish.
- QAT: fake-quantise weights and activations to the exact INT8 scales used by
  the shader, export per-layer scales, and measure quality after quantisation.

The training tool writes the neutral `XRU1` container:

```sh
python3 tools/train_upscaler.py \
  --model mlp --hidden 16 --scale 0.7 \
  --captures captures/highres/VRChat/scene \
  --output ~/.config/xrizer/upscaler_model.xru
```

`--self-test` validates the container and metadata without needing captures.
PyTorch is required for real training; see `tools/requirements-upscaler.txt`.

## Temporal and stereo evaluation

Capture **video**, not only stills. Use locked-camera slow pans, head micro-motion,
and real gameplay at the target scale. Process each frame independently, exactly
as shipping. `tools/eval_temporal.py` reports:

- tLPIPS (offline optical flow),
- warping error,
- IF-SSIM-style inter-frame similarity,
- per-pixel temporal variance.

Both eyes must use identical weights, tile geometry, and deterministic
processing. Evaluate left/right differences on mirrored content. A spatial
upscaler cannot remove input aliasing; residual clamping and L1/gradient
training are the primary flicker mitigations.

## Licensing and privacy

- XRizer is GPL-3.0-or-later.
- FSR 1 is MIT; SGSR 1 is BSD-3-Clause. Keep their notices.
- Do not use TrueFovea code, shaders, constants, weights, UI assets, or file
  formats.
- Do not redistribute captures without the participants' permission.
- Keep captures and models out of git; `.gitignore` covers `captures/`,
  `*.xru`, and generated model files.
