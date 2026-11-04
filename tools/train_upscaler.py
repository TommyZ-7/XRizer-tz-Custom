#!/usr/bin/env python3
"""Offline training/QAT/export for the XRizer spatial upscaler.

PyTorch is only needed for actual training. `--self-test` validates the XRU1
container and dataset metadata using the Python standard library, and runs a
tiny training step when PyTorch happens to be installed.

The XRU1 layout here must match src/upscaler/model.rs:

  header 64 bytes:
    magic "XRU1", version u16, kind u8, flags u8,
    feature_count u16, hidden u16, channels u16, layer_count u16,
    input_scale f32, activation_scale f32, residual_clamp f32, scale f32,
    zero padding to 64 bytes.
  layer table, 20 bytes per layer:
    input_count u32, output_count u32,
    weight_scale f32, input_scale f32, output_scale f32.
  then per layer: input_count*output_count int8 weights, output_count int32 biases.
"""

from __future__ import annotations

import argparse
import json
import math
import struct
import sys
import tempfile
from pathlib import Path

MAGIC = b"XRU1"
VERSION = 1
KIND = {"mlp": 0, "residual_cnn": 1, "srpo": 2}
HEADER_SIZE = 64
LAYER_ENTRY_SIZE = 20
MLP_FEATURES = 28


def pack_i8(value: float) -> int:
    return max(-127, min(127, int(round(value))))


def write_xru1(
    path: Path,
    kind: str,
    feature_count: int,
    hidden: int,
    channels: int,
    layers: list[dict],
    input_scale: float = 64.0,
    activation_scale: float = 1.0,
    residual_clamp: float = 0.15,
    scale: float = 1.0,
) -> None:
    header = bytearray()
    header += MAGIC
    header += struct.pack("<H", VERSION)
    header.append(KIND[kind])
    header.append(0)
    header += struct.pack("<HHHH", feature_count, hidden, channels, len(layers))
    header += struct.pack("<ffff", input_scale, activation_scale, residual_clamp, scale)
    header += bytes(HEADER_SIZE - len(header))

    table = bytearray()
    for layer in layers:
        table += struct.pack(
            "<IIfff",
            int(layer["input_count"]),
            int(layer["output_count"]),
            float(layer.get("weight_scale", 1.0 / 127.0)),
            float(layer.get("input_scale", 1.0)),
            float(layer.get("output_scale", 1.0)),
        )

    payload = bytearray()
    for layer in layers:
        weights = layer["weights"]
        expected = int(layer["input_count"]) * int(layer["output_count"])
        if len(weights) != expected:
            raise ValueError(f"layer has {len(weights)} weights, expected {expected}")
        payload += bytes((int(w) & 0xFF) for w in weights)
        biases = layer["biases"]
        if len(biases) != int(layer["output_count"]):
            raise ValueError("bias count does not match output_count")
        payload += struct.pack("<" + "i" * len(biases), *biases)

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(bytes(header + table + payload))


def read_xru1(path: Path) -> dict:
    data = path.read_bytes()
    if len(data) < HEADER_SIZE or data[:4] != MAGIC:
        raise ValueError("bad XRU1 header")
    version, kind, _flags, feature_count, hidden, channels, layer_count = struct.unpack_from(
        "<HBBHHHH", data, 4
    )
    if version != VERSION:
        raise ValueError(f"unsupported XRU1 version {version}")
    input_scale, activation_scale, residual_clamp, scale = struct.unpack_from("<ffff", data, 16)
    offset = HEADER_SIZE
    descriptors = []
    for _ in range(layer_count):
        input_count, output_count, weight_scale, layer_input_scale, layer_output_scale = (
            struct.unpack_from("<IIfff", data, offset)
        )
        offset += LAYER_ENTRY_SIZE
        descriptors.append(
            {
                "input_count": input_count,
                "output_count": output_count,
                "weight_scale": weight_scale,
                "input_scale": layer_input_scale,
                "output_scale": layer_output_scale,
            }
        )
    layers = []
    for descriptor in descriptors:
        count = descriptor["input_count"] * descriptor["output_count"]
        if offset + count > len(data):
            raise ValueError("truncated XRU1 weights")
        weights = [b if b < 128 else b - 256 for b in data[offset : offset + count]]
        offset += count
        bias_count = descriptor["output_count"]
        if offset + bias_count * 4 > len(data):
            raise ValueError("truncated XRU1 biases")
        biases = list(struct.unpack_from("<" + "i" * bias_count, data, offset))
        offset += bias_count * 4
        layers.append({**descriptor, "weights": weights, "biases": biases})
    if offset != len(data):
        raise ValueError("trailing XRU1 data")
    return {
        "version": version,
        "kind": kind,
        "feature_count": feature_count,
        "hidden": hidden,
        "channels": channels,
        "input_scale": input_scale,
        "activation_scale": activation_scale,
        "residual_clamp": residual_clamp,
        "scale": scale,
        "layers": layers,
    }


def validate_metadata(metadata: dict) -> None:
    required = ["game", "aa_mode", "color_space", "render_scale", "eye", "scene"]
    missing = [key for key in required if key not in metadata]
    if missing:
        raise ValueError(f"metadata is missing {missing}")
    if metadata["aa_mode"] not in {"none", "fxaa", "taa", "msaa"}:
        raise ValueError("unknown aa_mode")
    if metadata["color_space"] not in {"srgb", "linear", "hdr"}:
        raise ValueError("unknown color_space")
    if not (0.1 <= float(metadata["render_scale"]) <= 4.0):
        raise ValueError("render_scale out of range")


def self_test_mlp(hidden: int = 8) -> dict:
    first_weights = [((i * 7 + o * 13) % 31) - 15 for o in range(hidden) for i in range(MLP_FEATURES)]
    second_weights = [((i * 5 + 3) % 17) - 8 for i in range(hidden)]
    return read_xru1_bytes(
        build_bytes(
            "mlp",
            MLP_FEATURES,
            hidden,
            0,
            [
                {
                    "input_count": MLP_FEATURES,
                    "output_count": hidden,
                    "weights": first_weights,
                    "biases": [0] * hidden,
                },
                {
                    "input_count": hidden,
                    "output_count": 1,
                    "weights": second_weights,
                    "biases": [0],
                },
            ],
        )
    )


def build_bytes(
    kind: str,
    feature_count: int,
    hidden: int,
    channels: int,
    layers: list[dict],
    input_scale: float = 64.0,
    activation_scale: float = 1.0,
    residual_clamp: float = 0.15,
    scale: float = 1.0,
) -> bytes:
    with tempfile.NamedTemporaryFile(suffix=".xru", delete=False) as handle:
        path = Path(handle.name)
    try:
        write_xru1(
            path,
            kind,
            feature_count,
            hidden,
            channels,
            layers,
            input_scale,
            activation_scale,
            residual_clamp,
            scale,
        )
        return path.read_bytes()
    finally:
        path.unlink(missing_ok=True)


def read_xru1_bytes(data: bytes) -> dict:
    with tempfile.NamedTemporaryFile(suffix=".xru", delete=False) as handle:
        path = Path(handle.name)
        handle.write(data)
    try:
        return read_xru1(path)
    finally:
        path.unlink(missing_ok=True)


def try_import_torch():
    try:
        import torch  # type: ignore

        return torch
    except Exception:
        return None


def fake_quant_i8(tensor, scale: float):
    torch = try_import_torch()
    return torch.clamp(torch.round(tensor * scale), -127, 127) / scale


def quantize_linear(linear, input_scale: float):
    torch = try_import_torch()
    weights = torch.clamp(torch.round(linear.weight.detach().cpu() * 127.0), -127, 127).to(torch.int8)
    bias_scale = 127.0 * input_scale
    biases = (
        torch.clamp(
            torch.round(linear.bias.detach().cpu() * bias_scale),
            -(2**31),
            2**31 - 1,
        )
        .to(torch.int32)
    )
    return weights, biases


def charbonnier_loss(prediction, target, epsilon: float = 1e-3):
    torch = try_import_torch()
    return torch.sqrt((prediction - target) ** 2 + epsilon**2).mean()


def gradient_loss(prediction, target):
    torch = try_import_torch()
    pred_dx = prediction[..., :, 1:] - prediction[..., :, :-1]
    pred_dy = prediction[..., 1:, :] - prediction[..., :-1, :]
    target_dx = target[..., :, 1:] - target[..., :, :-1]
    target_dy = target[..., 1:, :] - target[..., :-1, :]
    return (pred_dx - target_dx).abs().mean() + (pred_dy - target_dy).abs().mean()


def tiny_train_step():
    torch = try_import_torch()
    if torch is None:
        print("torch not installed; skipping tiny train step")
        return
    torch.manual_seed(0)
    model = torch.nn.Sequential(
        torch.nn.Linear(MLP_FEATURES, 8),
        torch.nn.Hardtanh(-1.0, 1.0),
        torch.nn.Linear(8, 1),
        torch.nn.Hardtanh(-0.15, 0.15),
    )
    optimizer = torch.optim.Adam(model.parameters(), lr=1e-3)
    features = torch.randn(16, MLP_FEATURES)
    target = torch.zeros(16, 1)
    for _ in range(3):
        optimizer.zero_grad()
        prediction = model(features)
        loss = charbonnier_loss(prediction, target)
        loss.backward()
        optimizer.step()
    print(f"tiny train step ok, loss={loss.item():.6f}")


def load_metadata(captures: Path | None) -> dict:
    if captures is None:
        metadata = {
            "game": "self-test",
            "aa_mode": "taa",
            "color_space": "srgb",
            "render_scale": 0.7,
            "eye": "both",
            "scene": "synthetic",
        }
    else:
        path = captures / "metadata.json"
        if not path.exists():
            raise SystemExit(f"missing capture metadata: {path}")
        metadata = json.loads(path.read_text())
    validate_metadata(metadata)
    return metadata


def build_torch_model(args, torch):
    if args.model == "mlp":
        return torch.nn.Sequential(
            torch.nn.Linear(MLP_FEATURES, args.hidden),
            torch.nn.Hardtanh(-1.0, 1.0),
            torch.nn.Linear(args.hidden, 1),
            torch.nn.Hardtanh(-0.15, 0.15),
        )
    channels = args.channels or 16
    # The inference shader extracts a 3x3 RGB patch (27 features, padded to
    # 28) and applies three per-pixel INT8 layers. Keep the training topology
    # identical so the exported layer table is faithful.
    layers = [
        torch.nn.Linear(MLP_FEATURES, channels),
        torch.nn.Hardtanh(-1.0, 1.0),
        torch.nn.Linear(channels, channels),
        torch.nn.Hardtanh(-1.0, 1.0),
        torch.nn.Linear(channels, 3),
        torch.nn.Hardtanh(-0.25, 0.25),
    ]
    return torch.nn.Sequential(*layers)


def export_mlp(model, output: Path, hidden: int) -> None:
    torch = try_import_torch()
    if torch is None:
        raise SystemExit("torch is required to export a trained model")
    layers = []
    linear_layers = [m for m in model if isinstance(m, torch.nn.Linear)]
    for index, linear in enumerate(linear_layers):
        input_scale = 64.0 if index == 0 else 127.0
        weights, biases = quantize_linear(linear, input_scale)
        layers.append(
            {
                "input_count": int(weights.shape[1]),
                "output_count": int(weights.shape[0]),
                "weights": weights.flatten().tolist(),
                "biases": biases.tolist(),
                "weight_scale": 1.0 / 127.0,
                "input_scale": input_scale,
                "output_scale": 1.0,
            }
        )
    write_xru1(output, "mlp", MLP_FEATURES, hidden, 0, layers)


def export_cnn(model, output: Path, channels: int) -> None:
    torch = try_import_torch()
    if torch is None:
        raise SystemExit("torch is required to export a trained model")
    layers = []
    for linear in [m for m in model if isinstance(m, torch.nn.Linear)]:
        weights = linear.weight.detach().cpu()
        weights = fake_quant_i8(weights, 127.0).round().clamp(-127, 127).to(torch.int8)
        biases = (linear.bias.detach().cpu() * 127.0).round().clamp(-(2**31), 2**31 - 1)
        layers.append(
            {
                "input_count": int(weights.shape[1]),
                "output_count": int(weights.shape[0]),
                "weights": weights.flatten().tolist(),
                "biases": biases.to(torch.int32).tolist(),
                "weight_scale": 1.0 / 127.0,
                "input_scale": 64.0,
                "output_scale": 1.0,
            }
        )
    write_xru1(output, "residual_cnn", MLP_FEATURES, channels, channels, layers)


def export_srpo(model, output: Path, channels: int) -> None:
    torch = try_import_torch()
    if torch is None:
        raise SystemExit("torch is required to export a trained model")
    layers = []
    for linear in [m for m in model if isinstance(m, torch.nn.Linear)]:
        weights = linear.weight.detach().cpu()
        weights = fake_quant_i8(weights, 127.0).round().clamp(-127, 127).to(torch.int8)
        biases = (linear.bias.detach().cpu() * 127.0).round().clamp(-(2**31), 2**31 - 1)
        layers.append(
            {
                "input_count": int(weights.shape[1]),
                "output_count": int(weights.shape[0]),
                "weights": weights.flatten().tolist(),
                "biases": biases.to(torch.int32).tolist(),
                "weight_scale": 1.0 / 127.0,
                "input_scale": 64.0,
                "output_scale": 1.0,
            }
        )
    write_xru1(
        output,
        "srpo",
        MLP_FEATURES,
        channels,
        channels,
        layers,
        scale=1.5,
    )


def load_capture_pairs(captures: Path, scale: float):
    try:
        import numpy as np  # type: ignore
        from PIL import Image  # type: ignore
    except Exception as error:
        raise SystemExit(f"Pillow and NumPy are required for captures: {error}")
    paths = sorted(
        path
        for path in captures.rglob("*")
        if path.suffix.lower() in {".png", ".jpg", ".jpeg", ".bmp"}
    )
    if not paths:
        raise SystemExit(f"no capture images under {captures}")
    pairs = []
    for path in paths:
        rgb = np.asarray(Image.open(path).convert("RGB"), dtype=np.uint8)
        height, width = rgb.shape[:2]
        lr_width = max(8, int(round(width * scale)))
        lr_height = max(8, int(round(height * scale)))
        lr_image = Image.fromarray(rgb).resize((lr_width, lr_height), Image.BOX)
        base_image = lr_image.resize((width, height), Image.BILINEAR)
        hr = rgb.astype(np.float32) / 255.0
        base = np.asarray(base_image, dtype=np.float32) / 255.0
        pairs.append((base, hr, scale))
    return pairs


def extract_batch(pairs, model_kind: str, batch_size: int, patch: int = 8):
    import numpy as np  # type: ignore
    import random
    torch = try_import_torch()
    if torch is None:
        raise SystemExit("PyTorch is required for training")
    features = []
    targets = []
    while len(features) < batch_size:
        base, hr, scale = random.choice(pairs)
        height, width = hr.shape[:2]
        if height <= patch + 2 or width <= patch + 2:
            continue
        x = random.randint(1, width - patch - 1)
        y = random.randint(1, height - patch - 1)
        for yy in range(patch):
            for xx in range(patch):
                px = x + xx
                py = y + yy
                if model_kind == "mlp":
                    # 5x5 luma patch in LR space, mapped from the HR pixel.
                    lr_height = max(8, int(round(height * scale)))
                    lr_width = max(8, int(round(width * scale)))
                    lx = min(lr_width - 1, max(0, int(px * lr_width / width)))
                    ly = min(lr_height - 1, max(0, int(py * lr_height / height)))
                    luma = []
                    for dy in range(-2, 3):
                        for dx in range(-2, 3):
                            sx = min(lr_width - 1, max(0, lx + dx))
                            sy = min(lr_height - 1, max(0, ly + dy))
                            sample = base[sy * height // lr_height, sx * width // lr_width]
                            luma.append(
                                0.299 * sample[0] + 0.587 * sample[1] + 0.114 * sample[2]
                            )
                    phase_x = (px * lr_width / width) - int(px * lr_width / width)
                    phase_y = (py * lr_height / height) - int(py * lr_height / height)
                    features.append(luma + [phase_x, phase_y, scale])
                    target = hr[py, px] - base[py, px]
                    targets.append([0.299 * target[0] + 0.587 * target[1] + 0.114 * target[2]])
                else:
                    taps = base[py - 1 : py + 2, px - 1 : px + 2].reshape(-1).tolist()
                    taps.append(0.0)
                    features.append(taps)
                    targets.append((hr[py, px] - base[py, px]).tolist())
    return torch.tensor(features, dtype=torch.float32), torch.tensor(targets, dtype=torch.float32)


def train(args) -> None:
    torch = try_import_torch()
    if torch is None:
        raise SystemExit("PyTorch is required for training; install tools/requirements-upscaler.txt")
    if args.captures is None:
        raise SystemExit("--captures is required for training")
    metadata = load_metadata(args.captures)
    print(f"training {args.model} at scale {args.scale} for {metadata['game']}")
    pairs = load_capture_pairs(args.captures, args.scale)
    model = build_torch_model(args, torch)
    optimizer = torch.optim.Adam(model.parameters(), lr=args.learning_rate)
    for step in range(args.steps):
        features, target = extract_batch(pairs, args.model, args.batch_size)
        optimizer.zero_grad()
        prediction = model(features)
        loss = charbonnier_loss(prediction, target)
        loss = loss + 0.1 * (prediction - target).abs().mean()
        high_frequency_gain = torch.clamp(prediction.abs() - target.abs(), min=0.0).mean()
        loss = loss + 0.01 * high_frequency_gain
        loss.backward()
        optimizer.step()
        if args.qat:
            with torch.no_grad():
                for parameter in model.parameters():
                    parameter.copy_(fake_quant_i8(parameter, 127.0))
        if step % 50 == 0:
            print(f"step {step}: loss={loss.item():.6f}")
    output = Path(args.output)
    if args.model == "mlp":
        export_mlp(model, output, args.hidden)
    elif args.model == "residual_cnn":
        export_cnn(model, output, args.channels or 16)
    else:
        export_srpo(model, output, args.channels or 16)
    print(f"wrote {output}")

def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", choices=["mlp", "residual_cnn", "srpo"], default="mlp")
    parser.add_argument("--hidden", type=int, choices=[8, 16], default=16)
    parser.add_argument("--channels", type=int, default=16)
    parser.add_argument("--scale", type=float, default=0.7)
    parser.add_argument("--captures", type=Path)
    parser.add_argument("--output", type=Path, default=Path("upscaler_model.xru"))
    parser.add_argument("--steps", type=int, default=100)
    parser.add_argument("--batch-size", type=int, default=16)
    parser.add_argument("--learning-rate", type=float, default=1e-3)
    parser.add_argument("--qat", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv if argv is not None else sys.argv[1:])
    if args.self_test:
        metadata = load_metadata(None)
        validate_metadata(metadata)
        for hidden in (8, 16):
            parsed = self_test_mlp(hidden)
            assert parsed["feature_count"] == MLP_FEATURES
            assert parsed["hidden"] == hidden
            assert parsed["layers"][0]["output_count"] == hidden
            assert parsed["layers"][0]["biases"] == [0] * hidden
            assert parsed["layers"][1]["biases"] == [0]
        cnn_bytes = build_bytes(
            "residual_cnn",
            3,
            0,
            16,
            [
                {
                    "input_count": 3,
                    "output_count": 3,
                    "weights": [1, -1, 2, -2, 3, -3, 4, -4, 5],
                    "biases": [0, 0, 0],
                }
            ],
        )
        cnn = read_xru1_bytes(cnn_bytes)
        assert cnn["kind"] == KIND["residual_cnn"]
        print("XRU1 self-test ok")
        tiny_train_step()
        return 0
    train(args)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
