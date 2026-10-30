//! Which Telescope this is: the repo's VERSION file, as telescope/version.py reads it, for what's sent out (a User-Agent).

pub fn version() -> &'static str {
    include_str!("../../../../VERSION").trim()
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_is_a_version_number() {
        let v = super::version();
        assert!(v.split('.').count() >= 2 && v.split('.').all(|p| p.parse::<u32>().is_ok()), "{v}");
    }
}
