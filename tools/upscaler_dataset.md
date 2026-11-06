# Upscaler capture and training dataset

> **Historical:** this guide was written for the pre-B1a spatial upscaler.
> The current build is AA-only. Capture metadata now reports `output_scale:
> 1.0` and the AA method in the legacy `preset` field; ML/preset-specific
> instructions below are retained only for old datasets.

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
        frame_000001_left.png
        frame_000001_right.png
        frame_000002_left.png
        frame_000002_right.png
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

## XRizer capture mode

XRizer can write the submitted eye textures directly. This is opt-in and
disabled by default; without the environment variables no readback, copy, or
extra GPU work is created.

```sh
XRIZER_CAPTURE=1 \
XRIZER_CAPTURE_DIR="$HOME/xrizer-captures/VRChat/scene-a" \
XRIZER_CAPTURE_MODE=interval \
XRIZER_CAPTURE_INTERVAL_MS=500 \
XRIZER_CAPTURE_MAX_PAIRS=300 \
XRIZER_CAPTURE_SCENE="scene-a" \
  %command%
```

For controller-triggered captures:

```sh
XRIZER_CAPTURE=1 \
XRIZER_CAPTURE_DIR="$HOME/xrizer-captures/VRChat/scene-a" \
XRIZER_CAPTURE_MODE=shutter \
XRIZER_CAPTURE_MAX_PAIRS=300 \
  %command%
```

- `XRIZER_CAPTURE=1` (or setting `XRIZER_CAPTURE_DIR`) enables capture.
  `XRIZER_CAPTURE=0` disables it even if a directory is set.
- `XRIZER_CAPTURE_DIR` is the output directory. It is created if missing and
  may be a `captures/highres/<game>/<scene>` directory.
- `XRIZER_CAPTURE_MODE` selects `interval` (default) or `shutter`.
- `XRIZER_CAPTURE_INTERVAL_MS` defaults to 500 ms, clamped to 100–60000.
- In `shutter` mode, hold **both grips and both triggers** for 250 ms
  (`XRIZER_CAPTURE_HOLD_MS`, default 250, clamped 100–500). One pair is taken
  per press; a 1 s lockout (`XRIZER_CAPTURE_LOCKOUT_MS`, default 1000, clamped
  200–5000) rejects quick release/re-press. The detector does not consume or
  alter the game's input.
- Shutter detection piggybacks on the game's OpenXR action sync (VRChat and
  most action-manifest titles). Interval mode does not depend on input.
- `XRIZER_CAPTURE_MAX_PAIRS` defaults to 300 pairs (600 PNGs), clamped to
  1–100000. It applies to both modes; capture stops automatically. There is no
  VR hotkey.
- In `shutter` mode, a successful pair sends a best-effort freedesktop
  notification (D-Bus) with the frame number and save directory. If no
  notification server is available the notification is skipped; capture and
  gameplay continue. Interval mode does not notify.
- `XRIZER_CAPTURE_SCENE` is copied into the metadata (default `unspecified`).

Files are `frame_000001_left.png` and `frame_000001_right.png`, plus a
`metadata.json`. The writer uses lossless 8-bit RGBA PNG. The capture pass reads
the game's submitted texture before the upscaler and before runtime distortion.
If the source is HDR/10-bit it is clipped to 8-bit; MSAA submissions are skipped
because the capture shader samples a single-sample image. PNG encoding runs on a
background thread; the submit path records a separate command buffer and polls a
fence, it does not wait for disk I/O.

`metadata.json` contains:

```json
{
  "capture_version": 1,
  "game": "VRChat",
  "scene": "scene-a",
  "aa_mode": "unknown",
  "color_space": "unknown",
  "eye": "both",
  "session_started_unix_ms": 0,
  "render_scale": 0.7,
  "output_scale": 1.0,
  "preset": "fsr",
  "mode": "interval",
  "frames": [
    {
      "index": 1,
      "timestamp_unix_ms": 0,
      "frame_time_ms": 500.0,
      "render_scale": 0.7,
      "left": {"file": "frame_000001_left.png", "width": 2016, "height": 2240, "format": "R8G8B8A8_SRGB", "color_space": "srgb", "array_layer": 0, "bounds": [0.0, 0.0, 1.0, 1.0]},
      "right": {"file": "frame_000001_right.png", "width": 2016, "height": 2240, "format": "R8G8B8A8_SRGB", "color_space": "srgb", "array_layer": 0, "bounds": [0.0, 0.0, 1.0, 1.0]}
    }
  ]
}
```

The per-frame `index`, eye, dimensions, bounds, and `render_scale` are the
native-LR correlation keys. Keep the original `metadata.json` when moving a
capture session into the dataset tree.

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
