use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Difference {
    PhoneOlder,
    DesktopOlder,
    Different,
}

pub(crate) fn compare(phone: &str, desktop: &str) -> Option<Difference> {
    let (phone, desktop) = (phone.trim(), desktop.trim());
    if phone.is_empty() || desktop.is_empty() || phone == desktop {
        return None;
    }
    let order = match (nightly_build(phone), nightly_build(desktop)) {
        (Some(phone), Some(desktop)) => Some(phone.cmp(&desktop)),
        (None, None) => release(phone).zip(release(desktop)).map(|(phone, desktop)| phone.cmp(&desktop)),
        _ => None,
    };
    match order {
        Some(Ordering::Equal) => None,
        Some(Ordering::Less) => Some(Difference::PhoneOlder),
        Some(Ordering::Greater) => Some(Difference::DesktopOlder),
        None => Some(Difference::Different),
    }
}

fn release(version: &str) -> Option<[u64; 3]> {
    let parts = version.split('.').map(|part| {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) { return None; }
        part.parse().ok()
    }).collect::<Option<Vec<_>>>()?;
    parts.try_into().ok()
}

fn nightly_build(version: &str) -> Option<u64> {
    let (base, build) = version.split_once("-nightly.").or_else(|| version.split_once(" nightly "))?;
    release(base)?;
    if build.is_empty() || !build.bytes().all(|b| b.is_ascii_digit()) { return None; }
    build.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_release() {
        assert_eq!(compare("3.6.0", "3.6.0"), None);
    }

    #[test]
    fn older_phone() {
        assert_eq!(compare("3.9.0", "3.10.0"), Some(Difference::PhoneOlder));
        assert_eq!(compare("3.6.0", "3.6.1"), Some(Difference::PhoneOlder));
    }

    #[test]
    fn older_desktop() {
        assert_eq!(compare("4.0.0", "3.10.0"), Some(Difference::DesktopOlder));
    }

    #[test]
    fn nightlies_compare_build_numbers_in_both_formats() {
        assert_eq!(compare("3.6.0-nightly.123", "3.6.0 nightly 123"), None);
        assert_eq!(compare("3.5.0-nightly.123", "3.6.0 nightly 123"), None);
        assert_eq!(compare("3.6.0-nightly.99", "3.6.0 nightly 123"), Some(Difference::PhoneOlder));
        assert_eq!(compare("3.6.0-nightly.124", "3.6.0 nightly 123"), Some(Difference::DesktopOlder));
        assert_eq!(compare("3.6.0-nightly.124", "3.6.0-nightly.123"), Some(Difference::DesktopOlder));
    }

    #[test]
    fn release_and_nightly_only_differ() {
        assert_eq!(compare("3.6.0", "3.6.0 nightly 123"), Some(Difference::Different));
        assert_eq!(compare("3.6.0-nightly.123", "3.6.0"), Some(Difference::Different));
    }

    #[test]
    fn empty_phone_version_has_no_notice() {
        assert_eq!(compare("", "3.6.0"), None);
        assert_eq!(compare("  ", "3.6.0 nightly 123"), None);
    }

    #[test]
    fn unknown_formats_only_differ() {
        assert_eq!(compare("3.6.0-dev.1", "3.6.0 dev"), Some(Difference::Different));
        assert_eq!(compare("3.6.0-nightly.bad", "3.6.0 nightly 123"), Some(Difference::Different));
        assert_eq!(compare("unknown", "unknown"), None);
        assert_eq!(compare("3.6.0", ""), None);
    }
}
