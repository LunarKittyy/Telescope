//! shell-specimen OUT: every kit control kind by state, in the new look and in Classic, drawn offscreen as OUT/new.png and
//! OUT/classic.png, and the tooltip sheet as OUT/tips-new.png and OUT/tips-classic.png (see rust/crates/ui-kit/STYLE.md).

fn main() {
    let Some(out) = std::env::args().nth(1) else {
        eprintln!("usage: shell-specimen OUT");
        std::process::exit(2);
    };
    match telescope_shell::specimen::render(std::path::Path::new(&out)) {
        Ok(n) => println!("{n} pictures in {out}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
