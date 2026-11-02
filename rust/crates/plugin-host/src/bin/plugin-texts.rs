//! plugin-texts: a plugin's translation catalogs, like scripts/i18n.py --plugin does for Python plugins.
//!   plugin-texts extract FOLDER     finds the texts and writes locale/<plugin>.pot, updating every .po
//!   plugin-texts new LANG FOLDER    starts locale/<lang>.po
//!   plugin-texts status FOLDER      how far each language is

use std::path::Path;
use std::process::ExitCode;
use telescope_plugin_host::texts::{self, Target};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = |folder: &str, f: &dyn Fn(&Target) -> Result<String, String>| Target::new(Path::new(folder)).and_then(|t| f(&t));
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["extract", folder] => run(folder, &texts::extract),
        ["new", lang, folder] => run(folder, &|t| texts::new(t, lang)),
        ["status", folder] => run(folder, &texts::status),
        _ => {
            eprintln!("Usage: plugin-texts extract FOLDER | new LANG FOLDER | status FOLDER");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(said) => {
            println!("{said}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
