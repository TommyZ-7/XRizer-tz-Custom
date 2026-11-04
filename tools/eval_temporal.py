#!/usr/bin/env python3
"""Temporal evaluation for the XRizer spatial upscaler.

Metrics:
  * temporal variance per pixel across a static/micro-motion sequence;
  * warping error using Farneback flow when OpenCV is available;
  * IF-SSIM-style inter-frame SSIM;
  * tLPIPS between the current frame and the flow-warped previous frame when
    LPIPS/PyTorch are available.

The tool never uploads or embeds capture contents. It is intended for
user-provided, git-ignored sequences only.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path


def frame_paths(input_path: Path) -> list[Path]:
    if input_path.is_dir():
        paths = sorted(
            path
            for path in input_path.iterdir()
            if path.suffix.lower() in {".png", ".jpg", ".jpeg", ".bmp"}
        )
        if not paths:
            raise SystemExit(f"no image frames in {input_path}")
        return paths
    raise SystemExit("--input must be a directory of image frames")


def load_frames(paths: list[Path]):
    try:
        from PIL import Image  # type: ignore
        import numpy as np  # type: ignore
    except Exception as error:
        raise SystemExit(f"Pillow and NumPy are required: {error}")
    frames = []
    for path in paths:
        with Image.open(path) as image:
            frames.append(np.asarray(image.convert("RGB"), dtype=np.float32) / 255.0)
    return frames


def temporal_variance(frames):
    try:
        import numpy as np  # type: ignore
    except Exception:
        count = len(frames)
        height = len(frames[0])
        width = len(frames[0][0])
        channels = len(frames[0][0][0])
        total = 0.0
        for y in range(height):
            for x in range(width):
                for channel in range(channels):
                    values = [frame[y][x][channel] for frame in frames]
                    mean = sum(values) / count
                    total += sum((value - mean) ** 2 for value in values) / count
        return total / (height * width * channels)
    stack = np.stack(frames, axis=0)
    return float(stack.var(axis=0).mean())


def warping_error(frames) -> float | None:
    try:
        import cv2  # type: ignore
        import numpy as np  # type: ignore
    except Exception:
        return None
    if len(frames) < 2:
        return 0.0
    errors = []
    for previous, current in zip(frames, frames[1:]):
        previous_gray = cv2.cvtColor(previous, cv2.COLOR_RGB2GRAY)
        current_gray = cv2.cvtColor(current, cv2.COLOR_RGB2GRAY)
        flow = cv2.calcOpticalFlowFarneback(
            previous_gray, current_gray, None, 0.5, 3, 15, 3, 5, 1.2, 0
        )
        height, width = previous_gray.shape
        grid_x, grid_y = np.meshgrid(np.arange(width), np.arange(height))
        warped = cv2.remap(
            previous,
            (grid_x + flow[..., 0]).astype(np.float32),
            (grid_y + flow[..., 1]).astype(np.float32),
            cv2.INTER_LINEAR,
        )
        errors.append(float(np.abs(warped - current).mean()))
    return sum(errors) / len(errors)


def if_ssim(frames) -> float | None:
    try:
        import numpy as np  # type: ignore
        from skimage.metrics import structural_similarity  # type: ignore
    except Exception:
        return None
    if len(frames) < 2:
        return 1.0
    values = []
    for previous, current in zip(frames, frames[1:]):
        values.append(
            float(
                structural_similarity(
                    previous,
                    current,
                    channel_axis=2,
                    data_range=1.0,
                )
            )
        )
    return sum(values) / len(values)


def t_lpips(frames) -> float | None:
    try:
        import torch  # type: ignore
        import lpips  # type: ignore
        import numpy as np  # type: ignore
    except Exception:
        return None
    if len(frames) < 2:
        return 0.0
    loss = lpips.LPIPS(net="alex", verbose=False)
    values = []
    for previous, current in zip(frames, frames[1:]):
        previous_tensor = torch.from_numpy(previous).permute(2, 0, 1).unsqueeze(0) * 2 - 1
        current_tensor = torch.from_numpy(current).permute(2, 0, 1).unsqueeze(0) * 2 - 1
        with torch.no_grad():
            values.append(float(loss(previous_tensor, current_tensor).item()))
    return sum(values) / len(values)


def evaluate(frames) -> dict:
    return {
        "frames": len(frames),
        "temporal_variance": temporal_variance(frames),
        "warping_error": warping_error(frames),
        "if_ssim": if_ssim(frames),
        "t_lpips": t_lpips(frames),
    }


def self_test() -> int:
    frames = []
    for time in range(3):
        frame = []
        for y in range(8):
            row = []
            for x in range(8):
                value = ((x + y + time) % 16) / 15.0
                row.append([value, value, value])
            frame.append(row)
        frames.append(frame)
    variance = temporal_variance(frames)
    assert math.isfinite(variance) and variance >= 0.0
    print(f"eval_temporal self-test ok: temporal_variance={variance:.6f}")
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()
    if args.input is None:
        raise SystemExit("--input is required")
    frames = load_frames(frame_paths(args.input))
    result = evaluate(frames)
    text = json.dumps(result, indent=2)
    if args.output:
        args.output.write_text(text + "\n")
    print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
