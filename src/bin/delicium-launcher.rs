//! The stable entry point: starts the pet, keeps it and itself updated.
//!
//! Release builds use the Windows subsystem, so a double click shows no
//! console window; the launcher installs its own file logger once it knows
//! the installation root (see [`delicium::launcher::logging`]).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(error) = delicium::launcher::run() {
        // The logger may not exist yet: every failure that reaches here is
        // also written to the log when logging was initialised.
        eprintln!("Delicium: {error}");
        std::process::exit(1);
    }
}
