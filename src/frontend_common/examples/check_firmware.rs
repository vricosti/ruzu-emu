//! Read-only smoke test of the same preflight used by both launchers.
fn main() -> std::process::ExitCode {
    match frontend_common::firmware_manager::check_firmware_decryption() {
        Ok(()) => {
            println!("Installed firmware archives are readable (or no firmware is installed).");
            std::process::ExitCode::SUCCESS
        }
        Err(detail) => {
            eprintln!("Firmware preflight failed:\n{detail}");
            std::process::ExitCode::FAILURE
        }
    }
}
