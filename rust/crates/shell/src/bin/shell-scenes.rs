//! shell-scenes OUT [--scale 1]: the window in a few window states, drawn offscreen as
//! OUT/<scale>x/<scene>.png.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(out) = args.first() else {
        eprintln!("usage: shell-scenes OUT [--scale N]");
        std::process::exit(2);
    };
    let scale = args.iter().position(|a| a == "--scale").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    match telescope_shell::scenes::render_all(std::path::Path::new(out), scale) {
        Ok(n) => println!("{n} pictures in {out}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
