#![cfg_attr(windows, windows_subsystem = "windows")]

mod audio;
mod dialog;
mod save;
mod ui;
mod update;

use std::io;
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        ui_error(&error.to_string());
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Freshen starts this same executable as the helper. That has to happen
    // before the single-instance lock and before any window exists.
    if freshen::run_helper(std::env::args_os().skip(1))? {
        return Ok(());
    }
    let _instance = SingleInstance::acquire()?;
    dpi_aware();
    let root = install_root()?;
    let audio = audio::Audio::open(root.join("sounds"))?;
    let report = update::acknowledge_installation(&root)?;
    let library = save::Library::open(root.join("saves"))?;
    let handoff = ui::run(root, audio, library, report)?;
    drop(handoff);
    Ok(())
}

fn install_root() -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| io::Error::other("the program path has no directory"))?;
    directory.canonicalize()
}

fn dpi_aware() {
    unsafe {
        windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
}

fn ui_error(text: &str) {
    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let title: Vec<u16> = "BeepRS".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            0x40,
        );
    }
}

struct SingleInstance {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl SingleInstance {
    fn acquire() -> Result<Self, Box<dyn std::error::Error>> {
        let mut name: Vec<u16> = "Local\\BeepRS".encode_utf16().collect();
        name.push(0);
        // SAFETY: `name` is null-terminated UTF-16 and outlives this call.
        let handle = unsafe {
            windows_sys::Win32::System::Threading::CreateMutexW(std::ptr::null(), 1, name.as_ptr())
        };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe { windows_sys::Win32::Foundation::GetLastError() }
            == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS
        {
            // SAFETY: CreateMutexW returned this handle.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err("BeepRS is already running.".into());
        }
        Ok(Self { handle })
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateMutexW and has not been closed.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}
