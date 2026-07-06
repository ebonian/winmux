use winmux::{app, host};

fn main() {
    host::install_panic_hook();
    if let Err(e) = app::run() {
        eprintln!("winmux: {e}");
        std::process::exit(1);
    }
}
