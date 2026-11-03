//! check-plugin path/to/plugin: loads a plugin outside Telescope and puts it through what Telescope will do with it.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [folder] = args.as_slice() else {
        eprintln!("Usage: check-plugin path/to/your_plugin");
        return ExitCode::from(2);
    };
    let folder = std::path::Path::new(folder);
    let folder = telescope_core::system::plain_path(folder.canonicalize().unwrap_or_else(|_| folder.to_path_buf()));
    let report = telescope_plugin_host::testing::check(&folder);
    print!("{}", report.text());
    if report.problems() == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
