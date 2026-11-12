//! What the .slint files of an added plugin may reach: their own plugin folder and the ui-kit, nothing else. compile.rs asks this before
//! its file loader reads a file, because Slint reads imports, and `@image-url` images when they're drawn, from wherever a path points.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// The file every plugin folder has at its top (plugin-host's MANIFEST); the shell doesn't depend on that crate.
const PLUGIN_MANIFEST: &str = "plugin.json";

/// A name of a file that a .slint text gives as a string.
#[derive(Debug, PartialEq)]
pub enum Named {
    /// After `from` or `import`: another .slint file (or a font).
    Import(String),
    /// In `@image-url(...)`.
    Image(String),
    /// One of those, but built with a `\{...}` in it, so there is no telling what it is.
    Dynamic,
}

#[derive(Clone, Copy, PartialEq)]
enum Tok {
    None,
    Word(WordKind),
    Open,
    Other,
}

#[derive(Clone, Copy, PartialEq)]
enum WordKind {
    From,
    Import,
    ImageUrl,
    Plain,
}

/// An open string that a `\{` interrupted: what it says so far, what it names and how many `{` are open in the expression.
struct Open {
    bytes: Vec<u8>,
    names: Option<bool>,
    depth: usize,
}

/// Every string in a .slint text that names a file, read the way Slint's lexer reads comments and strings.
pub fn named_files(text: &str) -> Vec<Named> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let (mut prev, mut prev2) = (Tok::None, Tok::None);
    let mut open: Vec<Open> = Vec::new();
    let finish = |out: &mut Vec<Named>, names: Option<bool>, bytes: Vec<u8>, dynamic: bool| {
        let Some(import) = names else { return };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        out.push(match (dynamic, import) {
            (true, _) => Named::Dynamic,
            (false, true) => Named::Import(text),
            (false, false) => Named::Image(text),
        });
    };
    while i < b.len() {
        let c = b[i];
        if let Some(top) = open.last_mut() {
            if c == b'{' {
                top.depth += 1;
                i += 1;
                continue;
            }
            if c == b'}' {
                if top.depth > 0 {
                    top.depth -= 1;
                    i += 1;
                    continue;
                }
                i += 1;
                let mut s = open.pop().expect("there is an open string");
                match string_body(b, &mut i, &mut s.bytes) {
                    Body::Closed => {
                        finish(&mut out, s.names, s.bytes, true);
                        (prev2, prev) = (prev, Tok::Other);
                    }
                    Body::Interpolated => open.push(Open { depth: 0, ..s }),
                    Body::Unterminated => break,
                }
                continue;
            }
        }
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'/' && matches!(b.get(i + 1), Some(b'/' | b'*')) {
            match comment_length(&b[i..]) {
                0 => {
                    (prev2, prev) = (prev, Tok::Other);
                    i += 1;
                }
                n => i += n,
            }
        } else if c == b'"' {
            // What it names depends on the two tokens before it: `from "x"`, `import "x"` or `image-url("x"`
            let names = match (prev2, prev) {
                (_, Tok::Word(WordKind::From | WordKind::Import)) => Some(true),
                (Tok::Word(WordKind::ImageUrl), Tok::Open) => Some(false),
                _ => None,
            };
            i += 1;
            let mut bytes = Vec::new();
            match string_body(b, &mut i, &mut bytes) {
                Body::Closed => {
                    finish(&mut out, names, bytes, false);
                    (prev2, prev) = (prev, Tok::Other);
                }
                Body::Interpolated => open.push(Open { bytes, names, depth: 0 }),
                Body::Unterminated => break,
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'-') {
                i += 1;
            }
            let kind = match &text[start..i] {
                "from" => WordKind::From,
                "import" => WordKind::Import,
                "image-url" => WordKind::ImageUrl,
                _ => WordKind::Plain,
            };
            (prev2, prev) = (prev, Tok::Word(kind));
        } else {
            (prev2, prev) = (prev, if c == b'(' { Tok::Open } else { Tok::Other });
            i += 1;
        }
    }
    out
}

enum Body {
    Closed,
    Interpolated,
    Unterminated,
}

/// Reads a string's text up to its closing quote, or to a `\{` that starts an expression in it.
fn string_body(b: &[u8], i: &mut usize, out: &mut Vec<u8>) -> Body {
    while *i < b.len() {
        match b[*i] {
            b'"' => {
                *i += 1;
                return Body::Closed;
            }
            b'\\' => {
                let Some(&next) = b.get(*i + 1) else { return Body::Unterminated };
                *i += 2;
                match next {
                    b'{' => return Body::Interpolated,
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    other => out.push(other),
                }
            }
            c => {
                out.push(c);
                *i += 1;
            }
        }
    }
    Body::Unterminated
}

/// How long the comment at the start of `text` is (0 when it isn't one or never ends), by Slint's own rule: block comments nest.
fn comment_length(text: &[u8]) -> usize {
    if text.starts_with(b"//") {
        return text.iter().position(|c| *c == b'\n' || *c == b'\r').unwrap_or(text.len());
    }
    if !text.starts_with(b"/*") {
        return 0;
    }
    let (mut nested, mut at) = (0, 2);
    while at < text.len() {
        let Some(star) = text[at..].iter().position(|c| *c == b'*') else { return 0 };
        let star = star + at;
        if star > at && text[star - 1] == b'/' {
            nested += 1;
            at = star + 1;
        } else if star + 1 < text.len() && text[star + 1] == b'/' {
            if nested == 0 {
                return star + 2;
            }
            nested -= 1;
            at = star + 2;
        } else {
            at = star + 1;
        }
    }
    0
}

/// The path with `.` and `..` worked out and `\` read as `/`, without asking the disk (Slint's clean_path).
pub fn normalise(path: &Path) -> PathBuf {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = PathBuf::new();
    for part in Path::new(&text).components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The folder of the plugin a file belongs to: the nearest one above it with a plugin.json, else the file's own.
pub fn plugin_root(file: &Path) -> PathBuf {
    let own = file.parent().map(Path::to_path_buf).unwrap_or_default();
    own.ancestors().find(|dir| dir.join(PLUGIN_MANIFEST).is_file()).map(Path::to_path_buf).unwrap_or(own)
}

/// Whether path is inside root, by its name and, for what exists, by where it really is (a link can't lead out).
fn contained(path: &Path, root: &Path) -> bool {
    if !normalise(path).starts_with(normalise(root)) {
        return false;
    }
    match (path.canonicalize(), root.canonicalize()) {
        (Ok(real), Ok(base)) => telescope_core::system::plain_path(real).starts_with(telescope_core::system::plain_path(base)),
        _ => true,
    }
}

/// Which files the loader may read for a plugin, built up as the files it reads name others.
pub struct Reach {
    kit: PathBuf,
    library: String,
    /// A plugin file the loader may read, to the folder of the plugin it's in.
    allowed: RefCell<HashMap<PathBuf, PathBuf>>,
}

impl Reach {
    /// `trusted` is the generated document, whose imports of absolute paths are the plugins' own files the host picked.
    pub fn new(kit: &Path, library: &str, trusted: &str, shell_root: &Path) -> Reach {
        let reach = Reach { kit: normalise(kit), library: library.to_string(), allowed: RefCell::new(HashMap::new()) };
        for named in named_files(trusted) {
            let Named::Import(file) = named else { continue };
            let path = normalise(Path::new(&file));
            if path.is_absolute() && !path.starts_with(shell_root) && !reach.in_kit(&path) {
                let root = plugin_root(&path);
                reach.allowed.borrow_mut().insert(path, root);
            }
        }
        reach
    }

    pub fn in_kit(&self, path: &Path) -> bool {
        normalise(path).starts_with(&self.kit)
    }

    /// The plugin folder the loader may read path from, if it's a file the host picked or one a plugin file imported.
    pub fn plugin_folder(&self, path: &Path) -> Option<PathBuf> {
        let root = self.allowed.borrow().get(&normalise(path))?.clone();
        contained(path, &root).then_some(root)
    }

    /// Checks the text of the plugin file at `file` (in the plugin folder `root`): every file it names has to be inside the plugin or
    /// the ui-kit, and the .slint files it imports become readable. The error says which one isn't.
    pub fn vet(&self, file: &Path, root: &Path, text: &str) -> Result<(), String> {
        let here = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut imports = Vec::new();
        for named in named_files(text) {
            let (name, import) = match named {
                Named::Import(name) => (name, true),
                Named::Image(name) => (name, false),
                Named::Dynamic => return Err(format!("{here} builds a file name from an expression, which plugins can't do")),
            };
            if ["data:", "http://", "https://"].iter().any(|scheme| name.starts_with(scheme)) {
                continue;
            }
            if let Some(library) = name.strip_prefix('@') {
                let (lib, rest) = library.split_once('/').unwrap_or((library, ""));
                let climbs = Path::new(&rest.replace('\\', "/")).components().any(|c| matches!(c, Component::ParentDir));
                if lib != self.library || climbs {
                    return Err(format!("{here} uses {name:?}, which isn't a library plugins can use"));
                }
                continue;
            }
            let wanted = Path::new(&name);
            let path = if wanted.is_absolute() { normalise(wanted) } else { normalise(&file.parent().unwrap_or(Path::new("")).join(wanted)) };
            if !contained(&path, root) {
                return Err(format!("{here} uses {name:?}, which is outside the plugin"));
            }
            if import {
                imports.push(path);
            }
        }
        let mut allowed = self.allowed.borrow_mut();
        for path in imports {
            allowed.entry(path).or_insert_with(|| root.to_path_buf());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::testing::scratch_dir;

    fn import(s: &str) -> Named {
        Named::Import(s.into())
    }

    fn image(s: &str) -> Named {
        Named::Image(s.into())
    }

    #[test]
    fn the_files_a_text_names_are_found() {
        let text = "import { A } from \"a.slint\";\nimport \"f.ttf\";\nexport { B } from \"../b.slint\";\n\
                    export component C { Image { source: @image-url(\"x.png\"); } Image { source: @image-url( \"y.png\" , nine-slice(1)); } }";
        assert_eq!(named_files(text), [import("a.slint"), import("f.ttf"), import("../b.slint"), image("x.png"), image("y.png")]);
    }

    #[test]
    fn comments_and_other_strings_hide_and_show_nothing() {
        let text = "// import \"no.slint\"\n/* a /* nested */ import \"no.slint\" */ import { A } from /* c */ \"yes.slint\";\n\
                    export component C { Text { text: \"from \\\"no.slint\\\" \"; } Text { text: @tr(\"image-url(\\\"no.png\\\"\"); } }";
        assert_eq!(named_files(text), [import("yes.slint")]);
    }

    #[test]
    fn a_name_built_in_an_expression_is_dynamic() {
        assert_eq!(named_files("import { A } from \"a\\{x}.slint\"; export component C { Text { text: \"b\"; } }"), [Named::Dynamic]);
        assert_eq!(named_files("component C { Image { source: @image-url(\"a\\{ {1} }b.png\"); } }"), [Named::Dynamic]);
    }

    fn plugin_folders() -> (PathBuf, PathBuf, PathBuf) {
        let root = scratch_dir("reach");
        let (mine, other, kit) = (root.join("mine"), root.join("other"), root.join("kit"));
        for dir in [&mine, &other, &kit] {
            std::fs::create_dir_all(dir.join("sub")).unwrap();
        }
        for dir in [&mine, &other] {
            std::fs::write(dir.join(PLUGIN_MANIFEST), "{}").unwrap();
            std::fs::write(dir.join("card.slint"), "").unwrap();
        }
        (mine, other, kit)
    }

    #[test]
    fn a_plugin_reaches_its_own_folder_and_the_kit_only() {
        let (mine, other, kit) = plugin_folders();
        let card = mine.join("card.slint");
        let reach = Reach::new(&kit, "ui-kit", &format!("export {{ C }} from {:?};", card.to_string_lossy()), Path::new("/telescope-ui"));
        assert_eq!(reach.plugin_folder(&card), Some(mine.clone()));
        assert_eq!(reach.plugin_folder(&other.join("card.slint")), None);
        let ok = "import { A } from \"@ui-kit/ui-kit.slint\"; import { B } from \"sub/b.slint\"; import { C } from \"./sub/../c.slint\";\n\
                  component D { Image { source: @image-url(\"sub/x.png\"); } Image { source: @image-url(\"data:image/png;base64,AA\"); } }";
        assert_eq!(reach.vet(&card, &mine, ok), Ok(()));
        assert_eq!(reach.plugin_folder(&mine.join("sub/b.slint")), Some(mine.clone()));
        assert_eq!(reach.plugin_folder(&mine.join("c.slint")), Some(mine.clone()));
        for bad in ["import { A } from \"../other/card.slint\";", "import { A } from \"sub/../../other/card.slint\";",
                    "import { A } from \"/etc/passwd.slint\";", "import \"../other/font.ttf\";", "import { A } from \"@other/x.slint\";",
                    "import { A } from \"@ui-kit/../../etc/x.slint\";", "component D { Image { source: @image-url(\"../other/x.png\"); } }",
                    "component D { Image { source: @image-url(\"/etc/hostname\"); } }", "import { A } from \"sub\\\\..\\\\..\\\\other\\\\card.slint\";"] {
            let error = reach.vet(&card, &mine, bad).unwrap_err();
            assert!(error.contains("outside the plugin") || error.contains("isn't a library"), "{bad}: {error}");
        }
        assert!(reach.vet(&card, &mine, "component D { Image { source: @image-url(\"a\\{x}.png\"); } }").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_plugin_isnt_followed() {
        let (mine, other, kit) = plugin_folders();
        std::os::unix::fs::symlink(&other, mine.join("sneak")).unwrap();
        let card = mine.join("card.slint");
        let reach = Reach::new(&kit, "ui-kit", "", Path::new("/telescope-ui"));
        assert!(reach.vet(&card, &mine, "import { A } from \"sneak/card.slint\";").is_err());
    }
}
