// The specimen sheet draws both looks and its pictures aren't blank. Offscreen: nothing reaches the desktop.

#[test]
fn the_specimen_sheet_draws_both_looks() {
    let dir = std::env::temp_dir().join(format!("telescope-specimen-{}", std::process::id()));
    assert_eq!(telescope_shell::specimen::render(&dir), Ok(2));
    let mut sizes = Vec::new();
    for name in ["new.png", "classic.png"] {
        let bytes = std::fs::read(dir.join(name)).expect("the picture is there");
        assert!(bytes.starts_with(b"\x89PNG"), "{name} is a PNG");
        // A sheet with controls on it compresses far worse than a flat one
        assert!(bytes.len() > 50_000, "{name} has something on it: {} bytes", bytes.len());
        sizes.push(bytes);
    }
    assert_ne!(sizes[0], sizes[1], "the two looks differ");
    let _ = std::fs::remove_dir_all(&dir);
}
