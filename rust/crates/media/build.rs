// `ffmpeg` is set wherever this build links FFmpeg: always on Linux, on Windows with the ffmpeg-windows feature
fn main() {
    println!("cargo::rustc-check-cfg=cfg(ffmpeg)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os == "linux" || std::env::var_os("CARGO_FEATURE_FFMPEG_WINDOWS").is_some() {
        println!("cargo::rustc-cfg=ffmpeg");
    }
}
