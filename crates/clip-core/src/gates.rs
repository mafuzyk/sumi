//! Machine-readable record of the fidelity gates.
//!
//! `docs/FIDELITY_GATES.md` explains the vocabulary; this module makes the
//! claims executable so `clip gates` and the tests agree on what is verified
//! and what is unavailable. A claim is never both: an unavailable gate names
//! what is missing instead of passing silently.

/// The four gates. They answer different questions and are never merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Does the implementation do what its own contract says?
    Internal,
    /// Did a load/edit/save/reopen preserve the native container?
    NativeStructure,
    /// Does an independent application accept the file?
    ExternalAcceptance,
    /// Do pixels/geometry match an independent oracle within a tolerance?
    MeasuredParity,
}

impl Gate {
    pub fn code(self) -> &'static str {
        match self {
            Gate::Internal => "G1",
            Gate::NativeStructure => "G2",
            Gate::ExternalAcceptance => "G3",
            Gate::MeasuredParity => "G4",
        }
    }

    /// Short label for the report column.
    pub fn short(self) -> &'static str {
        match self {
            Gate::Internal => "internal",
            Gate::NativeStructure => "structure",
            Gate::ExternalAcceptance => "external",
            Gate::MeasuredParity => "parity",
        }
    }

    pub fn question(self) -> &'static str {
        match self {
            Gate::Internal => "does the implementation do what its contract says",
            Gate::NativeStructure => "did load/edit/save/reopen preserve the container",
            Gate::ExternalAcceptance => "does an independent application accept the file",
            Gate::MeasuredParity => "do pixels match an independent oracle within tolerance",
        }
    }
}

/// Whether a claim is backed by evidence in this tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Verified,
    Unavailable,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Verified => "verified",
            Status::Unavailable => "unavailable",
        }
    }
}

/// One claim: an area, the gate it uses, its evidence and its state.
#[derive(Debug, Clone, Copy)]
pub struct Claim {
    pub area: &'static str,
    pub gate: Gate,
    pub status: Status,
    /// Test function names that back a verified claim. The registry test scans
    /// the workspace for every name, so a stale citation fails the suite.
    pub tests: &'static [&'static str],
    /// What is missing, or where the evidence lives. Required when unavailable.
    pub note: &'static str,
}

/// Every claim this tree makes, in the order the review reads them.
pub const CLAIMS: &[Claim] = &[
        Claim {
        area: ".clip atomic save",
        gate: Gate::Internal,
        status: Status::Verified,
        tests: &["atomic_save_reopens_with_latest_pixels", "failed_save_preserves_existing_document"],
        note: "",
    },
        Claim {
        area: ".clip atomic save",
        gate: Gate::NativeStructure,
        status: Status::Verified,
        tests: &["second_save_keeps_latest_edits"],
        note: "",
    },
        Claim {
        area: "opaque native rows",
        gate: Gate::NativeStructure,
        status: Status::Verified,
        tests: &["editor_save_preserves_unrenderable_native_layer", "unsafe_edit_of_opaque_layer_is_rejected"],
        note: "",
    },
        Claim {
        area: "mask placement",
        gate: Gate::Internal,
        status: Status::Verified,
        tests: &["layer_mask_uses_independent_document_offset", "canvas_resize_keeps_mask_content_at_its_document_offset"],
        note: "",
    },
        Claim {
        area: "mask placement",
        gate: Gate::NativeStructure,
        status: Status::Verified,
        tests: &["folder_mask_survives_editor_save", "layer_mask_offset_survives_editor_save"],
        note: "",
    },
        Claim {
        area: "mask placement",
        gate: Gate::MeasuredParity,
        status: Status::Unavailable,
        tests: &[],
        note: "no CSP fixture for pass-through group masking",
    },
        Claim {
        area: "clipping base",
        gate: Gate::Internal,
        status: Status::Verified,
        tests: &["clipped_layer_over_empty_base_remains_invisible", "paper_does_not_change_clipping_coverage"],
        note: "",
    },
        Claim {
        area: "recovery",
        gate: Gate::Internal,
        status: Status::Verified,
        tests: &[
            "recovery_preserves_opaque_native_rows",
            "recovery_keeps_a_new_adjustment_layer_as_an_adjustment",
            "interrupted_publication_keeps_the_previous_recovery_listed",
        ],
        note: "",
    },
        Claim {
        area: "PSD/PSB",
        gate: Gate::Internal,
        status: Status::Verified,
        tests: &["external_psb_is_read_through_the_wide_length_fields", "psd_mask_survives_import_and_export", "layer_mask_round_trips_through_the_writer"],
        note: "",
    },
        Claim {
        area: "PDF export",
        gate: Gate::ExternalAcceptance,
        status: Status::Verified,
        tests: &["poppler_reports_the_exported_page_size_and_content"],
        note: "",
    },
        Claim {
        area: "colour proofing",
        gate: Gate::MeasuredParity,
        status: Status::Verified,
        tests: &[
            "pillow_reference_patches_match_for_every_intent_and_paper_mode",
            "paper_simulation_shows_the_simulated_sheet",
            "an_invalid_document_profile_is_rejected",
        ],
        note: "",
    },
        Claim {
        area: "colour adjustment LUTs",
        gate: Gate::MeasuredParity,
        status: Status::Verified,
        tests: &["color_balance_shadow_range_matches_csp"],
        note: "",
    },
        Claim {
        area: "brush fidelity",
        gate: Gate::MeasuredParity,
        status: Status::Unavailable,
        tests: &[],
        note: "no recorded CSP stroke or tablet trace to compare against",
    },
        Claim {
        area: "tablet input",
        gate: Gate::ExternalAcceptance,
        status: Status::Unavailable,
        tests: &[],
        note: "no physical pressure/tilt trace; Xvfb events are not tablet events",
    },
        Claim {
        area: "CLIP STUDIO PAINT acceptance",
        gate: Gate::ExternalAcceptance,
        status: Status::Unavailable,
        tests: &[],
        note: "recorded manually on 2026-11-08 (docs/CSP_ACCEPTANCE.md): CSP EX 5.1.5 under Wine opened a Sumi-written .clip, kept its layers and pixels, and its re-save renders pixel-identical; no automated test can drive CSP, and the run covers one raster-only document",
    },
        Claim {
        area: "Krita write-back",
        gate: Gate::ExternalAcceptance,
        status: Status::Unavailable,
        tests: &[],
        note: "no recorded Krita session that opens and re-saves the file",
    },
        Claim {
        area: "text and comic nodes",
        gate: Gate::Internal,
        status: Status::Unavailable,
        tests: &[],
        note: "authoring is raster only; no semantic node persistence",
    },
        Claim {
        area: "required default fixtures",
        gate: Gate::Internal,
        status: Status::Verified,
        tests: &["required_default_fixtures_are_present"],
        note: "",
    },
];

/// Print-ready report, one line per claim.
pub fn report() -> String {
    let mut out = String::new();
    for claim in CLAIMS {
        let detail = if claim.tests.is_empty() {
            claim.note.to_string()
        } else {
            claim.tests.join(", ")
        };
        out.push_str(&format!(
            "{} {:<13} {:<11} {:<28} {}\n",
            claim.gate.code(),
            claim.gate.short(),
            claim.status.label(),
            claim.area,
            detail
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_claim_is_complete() {
        assert!(!CLAIMS.is_empty());
        for claim in CLAIMS {
            assert!(!claim.area.trim().is_empty(), "a claim needs an area");
            match claim.status {
                Status::Verified => {
                    assert!(
                        !claim.tests.is_empty(),
                        "{} is verified but names no test",
                        claim.area
                    );
                    assert!(
                        claim.note.is_empty(),
                        "{} is verified but carries a missing note",
                        claim.area
                    );
                }
                Status::Unavailable => {
                    assert!(
                        claim.tests.is_empty(),
                        "{} is unavailable but claims tests",
                        claim.area
                    );
                    assert!(
                        !claim.note.trim().is_empty(),
                        "{} is unavailable but does not say what is missing",
                        claim.area
                    );
                }
            }
        }
    }

    /// Every source file of the workspace crates, joined into one buffer.
    fn workspace_sources() -> String {
        fn walk(path: &std::path::Path, out: &mut String) {
            let Ok(entries) = std::fs::read_dir(path) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        out.push_str(&text);
                        out.push('\n');
                    }
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root");
        let mut out = String::new();
        for krate in [
            "clip-core/src",
            "clip-core/tests",
            "csp-app/src",
            "csp-app/tests",
            "brush-engine/src",
            "clip-cli/src",
        ] {
            walk(&root.join(krate), &mut out);
        }
        out
    }

    #[test]
    fn every_named_test_exists_in_the_workspace() {
        // A registry that cites a test nobody wrote is worse than no registry:
        // it turns an unverified claim into an apparently evidenced one.
        let sources = workspace_sources();
        assert!(sources.len() > 10_000, "the workspace scan found no sources");
        for claim in CLAIMS {
            for name in claim.tests {
                let declared = format!("fn {name}(");
                assert!(
                    sources.contains(&declared),
                    "{} cites {name}, which no crate declares",
                    claim.area
                );
            }
        }
    }

    #[test]
    fn the_unavailable_gates_that_matter_are_recorded() {
        // Removing one of these from the registry would silently turn an
        // unavailable gate into an unmentioned one.
        for area in [
            "CLIP STUDIO PAINT acceptance",
            "tablet input",
            "brush fidelity",
            "Krita write-back",
        ] {
            let claim = CLAIMS
                .iter()
                .find(|claim| claim.area == area)
                .unwrap_or_else(|| panic!("{area} must stay in the registry"));
            assert_eq!(
                claim.status,
                Status::Unavailable,
                "{area} has no evidence in this tree"
            );
        }
    }

    #[test]
    fn the_report_names_every_claim_and_gate() {
        let report = report();
        assert_eq!(report.lines().count(), CLAIMS.len());
        for claim in CLAIMS {
            assert!(report.contains(claim.area), "{} missing from report", claim.area);
            assert!(report.contains(claim.gate.code()));
        }
        assert!(report.contains("unavailable"));
    }
}
