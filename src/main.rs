use winmux::{app, host};

fn main() {
    // Drop PSModulePath from our environment so pane shells inherit a clean
    // one. When winmux is launched from PowerShell 7, pwsh exports a
    // PSModulePath whose PS7 module directories precede the Windows
    // PowerShell 5.1 ones; a powershell.exe pane then resolves PSReadLine to
    // PS7's script-based module, which the default execution policy refuses
    // to load, and the pane prints "Cannot load PSReadline module. Console is
    // running without PSReadline." With the variable absent, each PowerShell
    // edition reconstructs its own correct default module path. (Trade-off:
    // a user-customized PSModulePath is not forwarded to panes.)
    //
    // Must happen before any pane spawns and while still single-threaded
    // (mutating the environment is not thread-safe on Windows).
    std::env::remove_var("PSModulePath");

    host::install_panic_hook();
    if let Err(e) = app::run() {
        eprintln!("winmux: {e}");
        std::process::exit(1);
    }
}
