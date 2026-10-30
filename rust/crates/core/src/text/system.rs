//! The fonts the system offers for a script, which is what Qt falls back on when the app's own fonts lack a character. Khmer or
//! Ethiopic text finds Noto Sans Khmer or Ethiopic wherever it is installed, without the app knowing its name.
//!
//! On Linux that is what Qt asks fontconfig (its sorted list of fonts for sans-serif in the script's language, the first with the
//! character wins); on Windows it is DirectWrite's system fallback, through fontique.

#[cfg(all(unix, not(target_vendor = "apple")))]
mod fontconfig;
#[cfg(not(all(unix, not(target_vendor = "apple"))))]
mod portable;

#[cfg(all(unix, not(target_vendor = "apple")))]
pub(super) use fontconfig::font_for;
#[cfg(not(all(unix, not(target_vendor = "apple"))))]
pub(super) use portable::font_for;

#[cfg(all(test, unix, not(target_vendor = "apple")))]
pub(super) use fontconfig::file_for;
