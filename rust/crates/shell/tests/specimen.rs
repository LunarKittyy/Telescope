// The specimen sheet draws every look and its pictures aren't blank. Offscreen: nothing reaches the desktop.

#[test]
fn the_specimen_sheet_draws_both_looks() {
    let dir = std::env::temp_dir().join(format!("telescope-specimen-{}", std::process::id()));
    assert_eq!(telescope_shell::specimen::render(&dir), Ok(8));
    let mut sizes = Vec::new();
    for name in ["new.png", "classic.png", "oled.png", "light.png", "tips-new.png", "tips-classic.png", "tips-oled.png", "tips-light.png"] {
        let bytes = std::fs::read(dir.join(name)).expect("the picture is there");
        assert!(bytes.starts_with(b"\x89PNG"), "{name} is a PNG");
        // A sheet with controls on it compresses far worse than a flat one (the tooltip sheet is smaller)
        let least = if name.starts_with("tips") { 8_000 } else { 50_000 };
        assert!(bytes.len() > least, "{name} has something on it: {} bytes", bytes.len());
        sizes.push(bytes);
    }
    for (a, b) in [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)] {
        assert_ne!(sizes[a], sizes[b], "the looks differ");
        assert_ne!(sizes[4 + a], sizes[4 + b], "and so do the tooltip sheets");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
