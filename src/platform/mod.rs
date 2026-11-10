//! The only platform-specific `unsafe` code in Chitra: letting a Windows
//! GUI-subsystem binary print to the console it was started from, and
//! printing photos through the Windows print dialog and GDI.

#[cfg(windows)]
pub mod print_windows;

/// On Windows, attach to the parent console so `chitra convert` output is visible
/// when run from cmd or PowerShell. No-op elsewhere.
#[cfg(windows)]
pub fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: AttachConsole takes a process id by value and has no memory-safety
    // preconditions; failure (no parent console) is harmless and ignored.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
pub fn attach_console() {}

/// Report a fatal startup error where the user can see it.
pub fn show_error(message: &str) {
    eprintln!("{message}");
    let _ = rfd::MessageDialog::new()
        .set_title("Chitra")
        .set_description(message)
        .set_level(rfd::MessageLevel::Error)
        .show();
}
