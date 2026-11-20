#!/usr/bin/env python3
"""Independent PSD/PSB check with psd-tools.

The port's own reader cannot prove interoperability: it can agree with the
port's own writer. This script reads a file with psd-tools (an independent
implementation) and reports the facts the port claims to write: stacking order,
visibility, offsets, opacity and blend mode.

Usage:
    psd_external_check.py FILE.psd [--expect-hidden NAME] [--expect-visible NAME]
                                   [--expect-offset NAME:X,Y]

Requires psd-tools (`uv pip install psd-tools`). Exit code 1 on a failed
expectation, on a NUL inside a layer name, or when psd-tools is missing.
"""

import argparse
import sys

try:
    from psd_tools import PSDImage
except ImportError:
    sys.exit("UNAVAILABLE: psd-tools is not installed; install it before citing this gate")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("file")
    parser.add_argument("--expect-hidden", action="append", default=[])
    parser.add_argument("--expect-visible", action="append", default=[])
    parser.add_argument("--expect-offset", action="append", default=[])
    args = parser.parse_args()

    psd = PSDImage.open(args.file)
    layers = {layer.name: layer for layer in psd}
    failures = []

    for name, layer in layers.items():
        bounds = layer.offset + (layer.offset[0] + layer.width, layer.offset[1] + layer.height)
        print(
            f"{name!r} visible={layer.visible} offset={layer.offset} "
            f"size={layer.width}x{layer.height} opacity={layer.opacity} "
            f"blend={layer.blend_mode} bounds={bounds}"
        )
        if "\x00" in name:
            failures.append(f"layer name {name!r} carries a NUL")

    for name in args.expect_hidden:
        if name not in layers:
            failures.append(f"expected hidden layer {name!r} is missing")
        elif layers[name].visible:
            failures.append(f"{name!r} must be hidden, psd-tools reports visible")

    for name in args.expect_visible:
        if name not in layers:
            failures.append(f"expected visible layer {name!r} is missing")
        elif not layers[name].visible:
            failures.append(f"{name!r} must be visible, psd-tools reports hidden")

    for spec in args.expect_offset:
        name, _, xy = spec.partition(":")
        want = tuple(int(part) for part in xy.split(","))
        if name not in layers:
            failures.append(f"expected layer {name!r} is missing")
        elif layers[name].offset != want:
            failures.append(f"{name!r} offset is {layers[name].offset}, expected {want}")

    for failure in failures:
        print(f"FAIL: {failure}", file=sys.stderr)
    print(f"layers={len(layers)} failures={len(failures)}")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
