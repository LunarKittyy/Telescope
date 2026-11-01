//! Puts the translations in the program: every <lang>.po and the template from the desktop app's locale folder, as bytes the language
//! loader reads (OUT_DIR/locale.rs). One file to ship and update, with no folder next to it to lose.

use std::path::{Path, PathBuf};

/// Where the catalogs live in the repository.
const LOCALE: &str = "../../desktop/telescope/locale";

/// With MSVC, FFmpeg's DLLs load on first use instead of at start, so losing one costs H.264 and not the whole program
/// (telescope_media::ffmpeg_available looks before anything calls them).
fn load_ffmpeg_late() {
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !msvc || std::env::var_os("CARGO_FEATURE_FFMPEG_WINDOWS").is_none() {
        return;
    }
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    let bin = PathBuf::from(std::env::var_os("FFMPEG_DIR").expect("FFMPEG_DIR names FFmpeg's folder")).join("bin");
    let names = std::fs::read_dir(&bin).expect("FFMPEG_DIR has a bin folder").flatten().map(|e| e.file_name().to_string_lossy().into_owned());
    for name in names.filter(|n| ["avcodec-", "avutil-", "swscale-"].iter().any(|p| n.starts_with(p)) && n.ends_with(".dll")) {
        println!("cargo::rustc-link-arg-bins=/DELAYLOAD:{name}");
    }
    println!("cargo::rustc-link-arg-bins=delayimp.lib");
}

/// Windows N has no Media Foundation until its media pack is added, and a DLL missing at start stops the program before anything can roll it
/// back. Webcams are all it's used for here (webcams_win::media_foundation_here looks before anything calls it).
fn load_media_foundation_late() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    for name in ["mfplat.dll", "mf.dll", "mfreadwrite.dll"] {
        println!("cargo::rustc-link-arg-bins=/DELAYLOAD:{name}");
    }
    println!("cargo::rustc-link-arg-bins=delayimp.lib");
}

/// Windows gives the main thread 1 MB where Linux gives 8, which a window this size goes through while it's built.
fn main_thread_stack() {
    let bytes = 8 * 1024 * 1024;
    match (std::env::var("CARGO_CFG_TARGET_OS").as_deref(), std::env::var("CARGO_CFG_TARGET_ENV").as_deref()) {
        (Ok("windows"), Ok("msvc")) => println!("cargo::rustc-link-arg-bins=/STACK:{bytes}"),
        (Ok("windows"), _) => println!("cargo::rustc-link-arg-bins=-Wl,--stack,{bytes}"),
        _ => {}
    }
}

/// On Linux a package built against FFMPEG_DIR carries that FFmpeg in lib/ next to the program.
fn find_shipped_ffmpeg() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") && std::env::var_os("FFMPEG_DIR").is_some() {
        println!("cargo::rustc-link-arg-bins=-Wl,-rpath,$ORIGIN/lib");
    }
}

/// The icon the tray and the window load as resource 1, the name Windows shows for the program, as telescope.spec's icon, and the version
/// Explorer shows, the same four numbers as the setup's VersionInfoVersion.
#[cfg(windows)]
fn program_resources() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    println!("cargo:rerun-if-changed=../../desktop/resources/telescope.ico");
    println!("cargo:rerun-if-changed=../../VERSION");
    println!("cargo:rerun-if-env-changed=TELESCOPE_BUILD");
    let version = std::fs::read_to_string("../../VERSION").expect("the repo's VERSION file");
    let mut parts = version.trim().split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    let build = std::env::var("TELESCOPE_BUILD").ok().and_then(|b| b.trim().parse().ok()).unwrap_or(0);
    let numbers = [parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0), build];
    let dotted = numbers.map(|n| n.to_string()).join(".");
    let packed = numbers.iter().fold(0u64, |all, n| (all << 16) | (n & 0xffff));
    let mut resources = winresource::WindowsResource::new();
    resources.set_icon("../../desktop/resources/telescope.ico")
        .set("ProductName", "Telescope").set("FileDescription", "Telescope").set("CompanyName", "LunarKittyy")
        .set("InternalName", "TelescopeDesktop").set("OriginalFilename", "TelescopeDesktop.exe")
        .set("FileVersion", &dotted).set("ProductVersion", &dotted)
        .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
    resources.compile().expect("the program's resources");
}

fn main() {
    #[cfg(windows)]
    program_resources();
    main_thread_stack();
    load_ffmpeg_late();
    load_media_foundation_late();
    find_shipped_ffmpeg();
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let folder = manifest.join(LOCALE);
    println!("cargo:rerun-if-changed={}", folder.display());
    println!("cargo:rerun-if-changed=build.rs");
    let include = |path: &Path| format!("include_bytes!({:?})", path.to_string_lossy().replace('\\', "/"));
    let mut catalogs: Vec<(String, PathBuf)> = std::fs::read_dir(&folder)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "po")).filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p))).collect())
        .unwrap_or_default();
    catalogs.sort();
    let template = folder.join("telescope.pot");
    if catalogs.is_empty() || !template.is_file() {
        println!("cargo:warning=no translations found in {}: the app will only be in English", folder.display());
    }
    let mut out = String::new();
    if template.is_file() {
        out += &format!("pub static TEMPLATE: &[u8] = {};\n", include(&template));
    } else {
        out += "pub static TEMPLATE: &[u8] = &[];\n";
    }
    out += "pub static CATALOGS: &[(&str, &[u8])] = &[\n";
    for (code, path) in &catalogs {
        out += &format!("    ({code:?}, {}),\n", include(path));
    }
    out += "];\n";
    let target = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets it")).join("locale.rs");
    std::fs::write(target, out).expect("write locale.rs");
}
