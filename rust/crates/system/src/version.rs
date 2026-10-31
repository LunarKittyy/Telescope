//! Which Telescope this is (telescope/version.py). A release build gets TELESCOPE_BUILD, TELESCOPE_CHANNEL and TELESCOPE_COMMIT at compile time; anything else is channel "dev".

pub const REPO: &str = "LunarKittyy/Telescope";

/// The repo's VERSION file, as the Python app reads it.
pub fn version() -> &'static str {
    telescope_core::version::version()
}

/// The commit count on master; 0 for a source build.
pub fn build() -> i64 {
    option_env!("TELESCOPE_BUILD").and_then(|b| b.trim().parse().ok()).unwrap_or(0)
}

pub fn channel() -> &'static str {
    option_env!("TELESCOPE_CHANNEL").filter(|c| !c.is_empty()).unwrap_or("dev")
}

pub fn commit() -> &'static str {
    option_env!("TELESCOPE_COMMIT").unwrap_or("")
}

/// What the UI shows: "3.0.0", "3.0.0 nightly 123" or "3.0.0 dev".
pub fn display_version() -> String {
    display_for(version(), channel(), build())
}

fn display_for(version: &str, channel: &str, build: i64) -> String {
    match channel {
        "stable" => version.to_string(),
        "dev" => format!("{version} dev"),
        other => format!("{version} {other} {build}"),
    }
}

/// Download link for a file from the release this build belongs to (dev builds use nightly).
pub fn release_asset_url(name: &str) -> String {
    asset_url_for(version(), channel(), name)
}

fn asset_url_for(version: &str, channel: &str, name: &str) -> String {
    let tag = if channel == "stable" { format!("v{version}") } else { "nightly".into() };
    format!("https://github.com/{REPO}/releases/download/{tag}/{name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn a_source_build_is_dev_with_the_repo_version() {
        assert_eq!(channel(), "dev");
        assert_eq!(build(), 0);
        assert_eq!(version().matches('.').count(), 2, "{}", version());
        assert_eq!(display_version(), format!("{} dev", version()));
        assert!(release_asset_url("x.apk").ends_with("/releases/download/nightly/x.apk"));
    }

    /// What a build of this file stamped with these says: the stamp is read at compile time, so this compiles it, with a stand-in for core's VERSION.
    fn stamped(channel: &str) -> Vec<String> {
        let dir = telescope_core::testing::scratch_dir(&format!("stamped-{channel}"));
        let core = dir.join("core.rs");
        std::fs::write(&core, "pub mod version { pub fn version() -> &'static str { \"1.2.3\" } }").unwrap();
        let this = concat!(env!("CARGO_MANIFEST_DIR"), "/src/version.rs");
        std::fs::write(dir.join("main.rs"), format!("#[path = {this:?}] mod version;\n\
            fn main() {{\n\
                println!(\"{{}}\", version::version());\n\
                println!(\"{{}}\", version::channel());\n\
                println!(\"{{}}\", version::build());\n\
                println!(\"{{}}\", version::commit());\n\
                println!(\"{{}}\", version::display_version());\n\
                println!(\"{{}}\", version::release_asset_url(\"Telescope.apk\"));\n\
            }}\n")).unwrap();
        let rustc = |args: &[&str]| {
            let out = Command::new("rustc").current_dir(&dir).args(args).env("TELESCOPE_BUILD", "99").env("TELESCOPE_CHANNEL", channel)
                .env("TELESCOPE_COMMIT", "abc").output().expect("rustc");
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        };
        rustc(&["--edition", "2021", "--crate-type", "rlib", "--crate-name", "telescope_core", "core.rs"]);
        rustc(&["--edition", "2021", "--extern", "telescope_core=libtelescope_core.rlib", "-A", "warnings", "-o", "stamped.exe", "main.rs"]);
        let out = Command::new(dir.join("stamped.exe")).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
    }

    #[test]
    fn a_stamped_build_reports_itself() {
        let stable = stamped("stable");
        assert_eq!(stable[..4], ["1.2.3", "stable", "99", "abc"]);
        assert_eq!(stable[4], "1.2.3");
        assert!(stable[5].ends_with("/releases/download/v1.2.3/Telescope.apk"), "{}", stable[5]);
        let nightly = stamped("nightly");
        assert_eq!(nightly[4], "1.2.3 nightly 99");
        assert!(nightly[5].ends_with("/releases/download/nightly/Telescope.apk"), "{}", nightly[5]);
    }
}
