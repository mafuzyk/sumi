//! Dump a synthetic output profile and the proof values this crate produces for
//! a few colours, so an independent implementation (Pillow ImageCms) can be
//! compared against the same inputs.
//!
//!     cargo run -p clip-core --example icc_reference -- /tmp/proof.icc

use lcms2::{CIExyY, Profile, ToneCurve};

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/sumi-proof.icc".to_string());
    // A D65-ish white point makes the simulated paper differ from the display
    // white, which is what "simulate paper colour" means for absolute
    // colorimetric proofing. The default is the D50 white point used by the
    // neutral fixture.
    let d65 = std::env::args().any(|arg| arg == "d65");
    let white = if d65 {
        CIExyY {
            x: 0.3127,
            y: 0.3290,
            Y: 1.0,
        }
    } else {
        CIExyY {
            x: 0.3457,
            y: 0.3585,
            Y: 1.0,
        }
    };
    let grey = Profile::new_gray(&white, &ToneCurve::new(2.2))?;
    let bytes = grey.icc()?;
    std::fs::write(&path, &bytes)?;
    println!("profile={path} bytes={}", bytes.len());

    let proof = clip_core::icc::Proof::new(&bytes, clip_core::icc::Intent::RelativeColorimetric)?;
    for source in [
        [30u8, 120, 220],
        [200, 40, 90],
        [128, 128, 128],
        [255, 0, 0],
    ] {
        let proofed = proof.proof_pixel(source);
        println!(
            "rust {:?} -> {:?}",
            source,
            proofed
        );
    }
    Ok(())
}
