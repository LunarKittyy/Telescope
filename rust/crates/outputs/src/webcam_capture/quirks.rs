//! What particular cameras do differently from the rest, in one place. Telescope never asks "is this a D5100?" anywhere else: the code
//! asks this table whether the camera has a quirk of a kind ("does aperture need live view to start again?") and acts on the answer.
//!
//! To add a camera: put it in `KNOWN` with the model name as libgphoto2 lists it (the name `gphoto2 --auto-detect` prints, which is
//! also what the Webcam card calls the camera), the quirks, and a note of where it was seen, and only once it has been confirmed on
//! the real camera. A guess costs everyone with that model a quirk they don't have. A new kind of quirk is a new `Quirk` variant, a
//! function below that answers it and a test in `webcam_capture/gphoto_lib.rs` that runs it against the fake camera.

/// One thing a camera is known to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quirk {
    /// The camera takes these settings (by the card's names for them) but the lens, sensor or sound only follows once live view has
    /// been started again, so the picture doesn't change until it is.
    LiveViewRestartFor(&'static [&'static str]),
}

/// A model and what it is known to do.
pub struct Entry {
    /// As libgphoto2 lists it, without a " (PTP mode)" or a "(2)" for a second one.
    pub model: &'static str,
    pub quirks: &'static [Quirk],
    /// Where it was seen: the camera and the date, so a later reader can tell a fact from a guess.
    pub confirmed: &'static str,
}

/// The cameras known to differ. Newer Nikons and Canons apply aperture live, so they are not here.
pub const KNOWN: &[Entry] = &[
    Entry {
        model: "Nikon DSC D5100",
        quirks: &[Quirk::LiveViewRestartFor(&["dslr_aperture"])],
        confirmed: "D5100 on the owner's desk, 2026-11-07: aperture was accepted but the picture changed only after live view restarted",
    },
];

/// The model without what libgphoto2 and the card add: " (PTP mode)" and the " (2)" of a second camera of the same model.
pub fn model_name(listed: &str) -> &str {
    let mut name = listed.trim();
    loop {
        let before = name;
        name = name.strip_suffix("(PTP mode)").map_or(name, str::trim_end);
        if let Some(open) = name.rfind(" (") {
            if name.ends_with(')') && name[open + 2..name.len() - 1].chars().all(|c| c.is_ascii_digit()) && name.len() > open + 3 {
                name = &name[..open];
            }
        }
        if name == before {
            return name;
        }
    }
}

/// What this camera is known to do.
pub fn quirks_of(model: &str) -> &'static [Quirk] {
    let name = model_name(model);
    KNOWN.iter().find(|e| e.model.eq_ignore_ascii_case(name)).map_or(&[], |e| e.quirks)
}

/// Whether a change to this setting (the card's name for it) only reaches the picture once live view starts again on this camera.
pub fn needs_live_view_restart(model: &str, setting: &str) -> bool {
    quirks_of(model).iter().any(|q| matches!(q, Quirk::LiveViewRestartFor(settings) if settings.contains(&setting)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn the_d5100_restarts_live_view_for_aperture_and_nothing_else_does() {
        assert!(needs_live_view_restart("Nikon DSC D5100", "dslr_aperture"));
        assert!(!needs_live_view_restart("Nikon DSC D5100", "dslr_exposure_comp"), "only what was seen");
        assert!(!needs_live_view_restart("Nikon DSC D5100", "dslr_white_balance"));
        for other in ["Nikon DSC D5200", "Nikon DSC D7500", "Canon EOS 600D", "Nikon", "", "Sony Alpha-A6000 (Control)"] {
            assert!(!needs_live_view_restart(other, "dslr_aperture"), "{other}");
            assert!(quirks_of(other).is_empty(), "{other}");
        }
    }

    #[test]
    fn a_model_is_found_however_it_is_listed() {
        for listed in ["Nikon DSC D5100", "Nikon DSC D5100 (PTP mode)", "Nikon DSC D5100 (2)", "Nikon DSC D5100 (PTP mode) (3)", "  nikon dsc d5100 "] {
            assert!(needs_live_view_restart(listed, "dslr_aperture"), "{listed}");
        }
        assert_eq!(model_name("Canon EOS 5D (Mark II)"), "Canon EOS 5D (Mark II)", "a name's own brackets stay");
    }

    #[test]
    fn every_entry_says_where_it_was_confirmed_and_has_a_quirk() {
        assert!(!KNOWN.is_empty());
        for e in KNOWN {
            assert!(e.confirmed.len() > 20 && !e.quirks.is_empty(), "{}", e.model);
            assert_eq!(model_name(e.model), e.model, "listed the way model_name leaves it");
        }
    }

    fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn no_model_is_named_outside_this_table_in_code_that_runs() {
        let mut files = Vec::new();
        sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let mut bad = Vec::new();
        for file in files {
            let name = file.to_string_lossy().replace('\\', "/");
            // This table, and tests, whose cameras are data
            if name.ends_with("/quirks.rs") || name.ends_with("/tests.rs") || name.ends_with("_tests.rs") {
                continue;
            }
            for (i, line) in std::fs::read_to_string(&file).unwrap().lines().enumerate() {
                let code = line.split("//").next().unwrap_or("").to_lowercase();
                // A test module inside a file: everything after it is tests
                if code.trim_start().starts_with("#[cfg(test)]") {
                    break;
                }
                for e in KNOWN {
                    let short = e.model.rsplit(' ').next().unwrap_or(e.model).to_lowercase();
                    if code.contains(&e.model.to_lowercase()) || code.contains(&short) {
                        bad.push(format!("{name}:{}: {}", i + 1, line.trim()));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "a model is named outside the quirks table:\n{}", bad.join("\n"));
    }
}
