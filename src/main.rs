// iron_renamer — batch file renamer.
// No arguments: launch the GUI. With arguments: CLI (see cli.rs / --help).

mod batch;
mod cli;
#[cfg(test)]
mod e2e;
mod engine;
mod gui;
mod presets;
mod tags;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        if let Err(e) = gui::run() {
            eprintln!("GUI error: {e}");
            std::process::exit(1);
        }
    } else {
        cli::run(args);
    }
}
