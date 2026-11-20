"""Regenerate testdata/proof_patches.json with an independent ICC engine.

Pillow's ImageCms wraps lcms2 too, but it is a separate implementation of the
calls and of the PNG/RGB plumbing, so agreement is evidence about the arguments
this crate passes (source, proof profile, both intents) rather than a replay of
our own code.

    python3 crates/clip-core/tests/proof_patches.py

Requires Pillow with ImageCms (13.0.0 was used) and
testdata/proof_bluish_paper.icc, which
`cargo run -p clip-core --example icc_reference -- <path> d65` writes.
"""

import json
import pathlib

from PIL import Image, ImageCms

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROFILE = ROOT / "testdata" / "proof_bluish_paper.icc"
OUTPUT = ROOT / "testdata" / "proof_patches.json"

COLOURS = [
    [30, 120, 220],
    [200, 40, 90],
    [128, 128, 128],
    [255, 0, 0],
    [255, 255, 255],
]
INTENTS = {
    "perceptual": 0,
    "relative": 1,
    "saturation": 2,
    "absolute": 3,
}


def main() -> None:
    proof = ImageCms.getOpenProfile(str(PROFILE))
    srgb = ImageCms.createProfile("sRGB")
    patches = []
    for name, intent in INTENTS.items():
        for paper in (False, True):
            proof_intent = 3 if paper else intent
            transform = ImageCms.buildProofTransform(
                srgb,
                srgb,
                proof,
                "RGB",
                "RGB",
                renderingIntent=intent,
                proofRenderingIntent=proof_intent,
            )
            values = []
            for colour in COLOURS:
                image = Image.new("RGB", (1, 1), tuple(colour))
                result = ImageCms.applyTransform(image, transform)
                values.append(list(result.convert("RGB").getpixel((0, 0))))
            patches.append(
                {
                    "intent": name,
                    "paper": paper,
                    "colours": COLOURS,
                    "values": values,
                }
            )
    OUTPUT.write_text(
        json.dumps(
            {
                "profile": PROFILE.name,
                "generator": "tests/proof_patches.py (Pillow ImageCms.buildProofTransform)",
                "patches": patches,
            },
            indent=1,
        )
        + "\n"
    )
    print(f"wrote {OUTPUT} with {len(patches)} patches")


if __name__ == "__main__":
    main()
