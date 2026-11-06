# xrizer - XR-ize your OpenVR games

[日本語](README.ja.md) | English

xrizer is a reimplementation of OpenVR on top of OpenXR. This enables you to run OpenVR games through any OpenXR runtime without running SteamVR.

Note that xrizer is currently immature. Many things are likely broken, so please open bugs! For a more mature solution, check out [OpenComposite](https://gitlab.com/znixian/OpenComposite), which some of the code in this repo is based on.

# Download & Usage

[Latest nightly](https://nightly.link/Supreeeme/xrizer/workflows/ci/main/xrizer-nightly-release.zip) - Bleeding edge!

[Latest release](https://github.com/Supreeeme/xrizer/releases/latest/download/xrizer-release.zip)

Choose a download and click it to download a zip. Unzip it and place it wherever you like.

In order to use xrizer, you must change where OpenVR games search for the runtime. There are two ways to accomplish this:

1. Add the path to xrizer to `$XDG_CONFIG_HOME/openvr/openvrpaths.vrpath`.
2. Set the `VR_OVERRIDE` environment variable when launching a game: `VR_OVERRIDE=/path/to/xrizer <command>` (steam launch options: `VR_OVERRIDE=/path/to/xrizer %command%`)
   - Note that if you do this method, you must have an existing valid `openvrpaths.vrpath` file, though xrizer doesn't need to be in it.

## openvrpaths.vrpath

This file is a JSON file read by OpenVR games to determine where the runtime is located. If you have SteamVR installed and have run it before, you can simply add the path to xrizer to the "runtime" section.
If you haven't, here is a sample file that can be placed in `$XDG_CONFIG_HOME/openvr/openvrpaths.vrpath` (be sure to update the path!):
```json
{
    "version": 1,
    "runtime": [
        "/path/to/xrizer"
    ]
}
```

## Steam Linux Runtime

When running games through the Steam Linux Runtime - which is all of them when using Proton 6 or newer - filesystem paths will not work the same way since it's a container. Some things to keep in mind are:
- Any path in your home directory will always be available
- Paths under `/usr` will be available at `/run/host/usr`
- Some paths will be completely unavailable.

You can make paths available within the container using the `PRESSURE_VESSEL_FILESYSTEMS_RW` env var. An example of what you might put in your game's steam launch option if you want to use xrizer with Monado installed systemwide is:
```
XR_RUNTIME_JSON=/run/host/usr/share/openxr/1/openxr_monado.json VR_OVERRIDE=/path/to/xrizer PRESSURE_VESSEL_FILESYSTEMS_RW=$XDG_RUNTIME_DIR/monado_comp_ipc
```
For more info on the container, see [Valve's docs on Pressure Vessel](https://gitlab.steamos.cloud/steamrt/steam-runtime-tools/-/blob/main/pressure-vessel/wrap.1.md).

# FAQ

## What games work on xrizer?

You tell me! The aim is for all standard (non overlay/utility/background) OpenVR apps to function as they would on SteamVR. Obviously this is not 100% the case, so open issues as you run into games that don't work properly and they will be addressed in time.

## Why rewrite OpenComposite?

OpenComposite has several years of existence over xrizer, so rewriting it is no small task. However, OpenComposite also lacks sufficient testing infrastructure, making it easy to inadvertently introduce regressions, and the way it's architected makes it difficult to write simple tests. OpenComposite was also not originally designed to utilize OpenXR, and there's still some legacy stuff from that period remaining in the codebase, which can make it more convoluted to understand. Dealing with these issues for a while led me to conclude that it would be more productive to rewrite it.

## Why Rust?

I like Rust. I don't like CMake.

# Building
```
# dev build
cargo xbuild

# release build
cargo xbuild --release
```

After building, the output directory can be used as a runtime directory. If you built the dev build, this will be `<path to xrizer repo>/target/debug`, and for the release build this is `<path to xrizer repo>/target/release`.

## Arch Linux — Build & Install

This fork adds dynamic resolution (`supersampleScale`) support. Tested on Arch Linux (2026-09).

### 1. Dependencies
```sh
sudo pacman -S base-devel rust clang llvm pkgconf glslang vulkan-headers openxr
# rust >=1.85 required (see rust-version in Cargo.toml)
# glslang provides glslc for shader compilation
# alternative: rustup + toolchain 1.85
```

If `glslc --version` is missing, `glslang` was not installed.

### 2. Build

```sh
git clone https://github.com/TommyZ-7/xrizer.git
cd xrizer
# optional: checkout feature branch
git checkout feature/dynamic-resolution

cargo xbuild              # debug -> target/debug/libxrizer.so
cargo xbuild --release    # release -> target/release/libxrizer.so
# output is also available as target/(debug|release)/bin/linux64/vrclient.so -> libxrizer.so
```

No extra Cargo features required for dynamic resolution; `monado` (default) is enough for WiVRn/Monado.

### 3. Install / Use

Same as upstream — pick one:

**a) VR_OVERRIDE (recommended for Steam/Proton, no system config):**
Steam > VRChat > Properties > Launch Options:
```
VR_OVERRIDE=/home/$USER/xrizer/target/release %command%
```
Requires existing `$XDG_CONFIG_HOME/openvr/openvrpaths.vrpath` (create empty JSON if needed, see below).

**b) openvrpaths.vrpath:**
Add runtime path to `$XDG_CONFIG_HOME/openvr/openvrpaths.vrpath`:
```json
{
  "version": 1,
  "runtime": ["/home/USER/xrizer/target/release"]
}
```

**With WiVRn (Monado) on Arch (system-wide):**
```
XR_RUNTIME_JSON=/run/host/usr/share/openxr/1/openxr_wivrn.json \
VR_OVERRIDE=/home/$USER/xrizer/target/release \
PRESSURE_VESSEL_FILESYSTEMS_RW=$HOME/.config \
%command%
```
`PRESSURE_VESSEL_FILESYSTEMS_RW` shares the scale file into the Proton container (required for dynamic resolution via file).

Verify runtime:
```sh
RUST_LOG=info VR_OVERRIDE=/home/$USER/xrizer/target/release vrchat 2>&1 | grep xrizer
# log: $XDG_STATE_HOME/xrizer/xrizer.txt
```

## Dynamic Resolution (VRChat) — This Fork Only

Upstream xrizer ignores `IVRSettings::SetFloat("steamvr","supersampleScale")`. This fork implements it and scales `IVRSystem::GetRecommendedRenderTargetSize` so VRChat (Unity OpenVR, OVRDR-tested Working) reallocates its RenderTexture without restart. `compositor` reuses or recreates swapchains (`src/compositor.rs:1244` `is_usable_swapchain`).

### How it works
* `src/settings.rs:84,148` — `SetFloat/GetFloat` for `steamvr/supersampleScale` (clamped 0.1–4.0) + `SetBool/SetInt32/GetBool/GetInt32` for `supersampleManualOverride` (OVRDR compatibility).
* `src/system.rs:211` — `GetRecommendedRenderTargetSize` returns `recommended * scale`.
* Cross-process (Proton vs host) via file poll (`src/settings.rs:28` `maybe_reload_scale` every 500 ms). `SetFloat` also persists to file so external tools propagate.

### Control methods

**1) Manual — echo to file (simplest, always works in container):**
```sh
echo 0.7 > ~/.config/xrizer_scale        # 70%  (0.7 or 70 both accepted, >4 interpreted as percent)
echo 150 > ~/.config/xrizer_scale        # 150% -> 1.5
echo 1.0 > ~/.config/xrizer_scale        # reset
# file polled within 500 ms, VRChat resizes next GetRecommendedRenderTargetSize
# alternative paths checked in order: $XRIZER_SCALE_FILE, $XDG_RUNTIME_DIR/xrizer_scale, ~/.config/xrizer_scale, ~/.local/share/xrizer_scale, /tmp/xrizer_scale
```

**2) Environment override:**
```sh
XRIZER_SCALE_FILE=/tmp/my_scale VR_OVERRIDE=... %command%
echo 0.8 > /tmp/my_scale
```

**3) OpenVR API — OVRDR / custom tool (automatically file-backed):**
```cpp
vr::VRSettings()->SetFloat(vr::k_pch_SteamVR_Section, vr::k_pch_SteamVR_SupersampleScale_Float, 0.7f);
// or for OVRDR: build https://github.com/Erimelowo/OpenVR-Dynamic-Resolution and run — it calls SetFloat internally
vr::VRSystem()->GetRecommendedRenderTargetSize(&w,&h); // verify w/h scaled
```
OVRDR WorkingGames.md lists VRChat as Working; with this fork OVRDR adjusts VRChat resolution based on GPU/CPU frametime without restart.

**Notes:**
* Clamp 0.1–4.0 (0.5 lower bound recommended to keep UI readable). Expand (> current) causes `recreating swapchain` log + ~30 ms hitch; shrink reuses swapchain.
* WiVRn encode resolution is separate; lowering VRChat + WiVRn scale together gives max saving.
* Debug: `RUST_LOG=openvr_calls,system=debug cargo xbuild` then `grep supersampleScale ~/.local/state/xrizer/xrizer.txt`.

## Display Brightness (Game Layers Only) — This Fork Only

Dims only the game projection layer via `XR_KHR_composition_layer_color_scale_bias` (`src/compositor.rs` `end_frame`). Overlays (WayVR, etc.) stay bright. `1.0` = normal, `0.5` = half, `0.0` = black. Clamped 0.0–1.0. Requires a runtime with the extension (WiVRn v26.9 / Monado builds with it ON); without it brightness is silently ignored.

```sh
echo 0.5 > ~/.config/xrizer_brightness   # half (0.5 or 50 both accepted, >1 interpreted as percent)
echo 1.0 > ~/.config/xrizer_brightness   # reset
# same file order/env override style as xrizer_scale: $XRIZER_BRIGHTNESS_FILE, $XDG_RUNTIME_DIR/xrizer_brightness, ~/.config/xrizer_brightness, ~/.local/share/xrizer_brightness, /tmp/xrizer_brightness
```

```cpp
vr::VRSettings()->SetFloat("xrizer", "brightness", 0.5f); // also persisted to file
```

## Experimental Antialiasing (Vulkan) — This Fork Only

Default off. The config lives at `~/.config/xrizer/upscaler.json` (override with `XRIZER_UPSCALER_CONFIG`); it is hot-reloaded every 500 ms and a broken file keeps the last good settings.

This fork is now AA-only. It never resizes the submitted eye image; the output extent equals the input extent. The upscaler presets, EASU, SGSR, ML model, and Vulkan feature layer were removed in B1a. B1b implements the equal-size FXAA pass. CMAA2 and SMAA 1x follow in B2 and B3; until then `cmaa2` and `smaa` fall back to FXAA. The B1b build defaults to `fxaa`; B2 changes the default to `cmaa2`.

The radial weight field has three modes:

- `center`: maximum at the centre, zero outside `radius + feather` (the old `field_weight`).
- `inverted`: zero inside `radius`, maximum at the periphery (default).
- `whole`: uniform weight over the whole image; `center`, `center_offset`, `radius`, and `feather` are ignored.

`center_offset` moves the field outward for the left eye and inward for the right eye (or vice versa depending on sign), matching the per-eye projection asymmetry seen in the 0.6.4 tests.

Example `~/.config/xrizer/upscaler.json`:

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

The old keys `preset`, `output_scale`, `model_path`, and the ML keys are ignored and preserved as unknown keys. `auto_scale`, `min_scale`, and `max_scale` still control the game-side render scale; they are not part of the AA pass.

Diagnostics are counters only (no image contents): frames, AA frames, FXAA/CMAA2/SMAA counts, fallback count, and the last fallback reason. A summary is logged every 300 frames to `$XDG_STATE_HOME/xrizer/xrizer.txt`. MSAA submissions take the existing resolve path.

### Training captures (opt-in)

For Task 14 training data, XRizer can save the submitted eye textures (before upscaling and before runtime distortion) as lossless 8-bit PNGs. Capture is disabled unless explicitly enabled, so the normal path performs no readback or extra copy:

```sh
XRIZER_CAPTURE=1 \
XRIZER_CAPTURE_DIR="$HOME/xrizer-captures/VRChat/scene-a" \
XRIZER_CAPTURE_MODE=interval \
XRIZER_CAPTURE_INTERVAL_MS=500 \
XRIZER_CAPTURE_MAX_PAIRS=300 \
  %command%
```

For controller-triggered captures use `XRIZER_CAPTURE_MODE=shutter`: hold **both grips and both triggers** for 250 ms. One pair is taken per press, with a 1 s lockout (`XRIZER_CAPTURE_HOLD_MS` / `XRIZER_CAPTURE_LOCKOUT_MS`). The chord is read through XRizer's OpenXR actions and is not consumed, so the game still receives all buttons. Shutter detection piggybacks on the game's action sync (VRChat and most action-manifest titles); interval mode does not depend on input.

Files are `frame_000001_left.png` and `frame_000001_right.png` plus `metadata.json` (resolution, format, colour space, bounds, `render_scale`, preset, per-eye entries). Capture stops automatically at `XRIZER_CAPTURE_MAX_PAIRS` (default 300 pairs); there is no VR hotkey. `XRIZER_CAPTURE=0` is the off-switch. In shutter mode a successful pair also sends a best-effort freedesktop notification with the frame number and save directory; failure to reach a notification server never blocks capture or gameplay. See `tools/upscaler_dataset.md` for the full layout and metadata. PNG encoding is off the submit path; the submit thread only records a separate command buffer and polls a fence. MSAA is skipped and HDR/10-bit sources are clipped to 8-bit.

The version xrizer reports comes from `git describe`, via the default `git-version` feature. Builds that don't have a git checkout - distribution packages and other builds from a source tarball - can disable that feature with `--no-default-features` (re-enabling the other default features they want, e.g. `--features monado`) and set `XRIZER_VERSION` at build time instead. `XRIZER_VERSION` takes precedence over `git describe` whenever it is set.

# Contributing

All contributions welcome.
- If you're opening a bug, please submit a log. The log is located at `$XDG_STATE_HOME/xrizer/xrizer.txt`, or `$HOME/.local/state/xrizer/xrizer.txt` if `$XDG_STATE_HOME` is not set.
- If submitting pull requests, please consider writing a test if possible/helpful - OpenVR is a large API surface and games are fickle, so ensuring things are well tested prevents future unintentional breakage.

# Environment Variables
_RUST_LOG_ - This is used for adjusting the logging of xrizer. See the [env_logger documentation](https://docs.rs/env_logger/latest/env_logger/#enabling-logging) for understanding how this works. Here are some useful nonstandard logging targets:
- `openvr_calls` - logs the name of each OpenVR function as they are called
- `tracked_property` - logs the name and device index of each requested tracked device property.

_XRIZER_CUSTOM_BINDINGS_DIR_ - This can be used to supply a directory that xrizer will search for controller bindings files. Note that the format of these bindings aren't actually documented anywhere, but it's easy enough to modify an existing file, and xrizer parses them so you can read the source too.

_XRIZER_TRACKER_SERIALS_ - This is a semi-colon (`;`) separated list of device serial numbers to use as generic trackers. Can be used to assign controllers as FBT trackers.

# See also

- [OpenComposite](https://gitlab.com/znixian/OpenOVR) - The original OpenVR/OpenXR implementation, much more mature than xrizer. Some of the code in this repo was rewritten based on OpenComposite.
- [OpenVR](https://github.com/ValveSoftware/openvr)
- [Monado](https://gitlab.freedesktop.org/monado/monado) - An open source OpenXR runtime
- [WiVRn](https://github.com/WiVRn/WiVRn) - An OpenXR streaming runtime for standalone headsets, based on Monado.
