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

    #[test]
    fn a_source_build_is_dev_with_the_repo_version() {
        assert_eq!(channel(), "dev");
        assert_eq!(build(), 0);
        assert!(version().split('.').count() >= 3);
        assert_eq!(display_version(), format!("{} dev", version()));
        assert!(release_asset_url("x.apk").ends_with("/releases/download/nightly/x.apk"));
    }

    #[test]
    fn a_stamped_build_reports_itself() {
        assert_eq!(display_for("1.2.3", "stable", 99), "1.2.3");
        assert!(asset_url_for("1.2.3", "stable", "Telescope.apk").ends_with("/releases/download/v1.2.3/Telescope.apk"));
        assert_eq!(display_for("1.2.3", "nightly", 99), "1.2.3 nightly 99");
    }
}
