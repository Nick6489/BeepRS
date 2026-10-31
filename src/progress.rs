//! Native update controls and a presentation-only process outside Inno's files.
#![allow(unsafe_op_in_unsafe_fn)]

use std::os::windows::process::CommandExt;
use std::{
    fs, io,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM},
    System::{
        LibraryLoader::GetModuleHandleW,
        Threading::{CREATE_NO_WINDOW, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    },
    UI::{
        Controls::{
            ICC_PROGRESS_CLASS, INITCOMMONCONTROLSEX, InitCommonControlsEx, PBM_SETMARQUEE,
            PBM_SETPOS, PBM_SETRANGE32, PBS_MARQUEE,
        },
        Input::KeyboardAndMouse::EnableWindow,
        WindowsAndMessaging::{
            DialogBoxParamW, EndDialog, GWL_STYLE, GetDlgItem, GetWindowLongPtrW, GetWindowLongW,
            KillTimer, SendMessageW, SetTimer, SetWindowLongPtrW, SetWindowLongW, SetWindowTextW,
            WM_CLOSE, WM_COMMAND, WM_INITDIALOG, WM_TIMER,
        },
    },
};

pub const UPDATE_DIALOG: u16 = 201;
// DWLP_USER follows a pointer-sized message result and DLGPROC in DLGWINDOWEXTRA.
const DWLP_USER: i32 = (2 * std::mem::size_of::<isize>()) as i32;
const MONITOR_DIALOG: u16 = 202;
const BAR: i32 = 122;
const LABEL: i32 = 123;
const OPEN: u16 = 124;
const CANCEL: u16 = 2;

pub fn initialize() -> io::Result<()> {
    let controls = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_PROGRESS_CLASS,
    };
    if unsafe { InitCommonControlsEx(&controls) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Remove only our known presentation files once Windows releases the copied image.
pub fn cleanup() {
    let Ok(root) = crate::update::servicing_root() else {
        return;
    };
    let Ok(entries) = fs::read_dir(root.join("progress")) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let directory = entry.path();
        let executable = directory.join("progress.exe");
        // An active monitor's image is locked. Leave its entire context alone.
        if executable.exists() && fs::remove_file(executable).is_err() {
            continue;
        }
        let _ = fs::remove_file(directory.join("application.json"));
        let _ = fs::remove_file(directory.join("ready"));
        let _ = fs::remove_dir(directory);
    }
}

pub fn show(hwnd: HWND, percent: Option<u32>, text: &str) {
    unsafe {
        let bar = GetDlgItem(hwnd, BAR);
        let style = GetWindowLongW(bar, GWL_STYLE) as u32;
        if let Some(percent) = percent {
            SendMessageW(bar, PBM_SETMARQUEE, 0, 0);
            SetWindowLongW(bar, GWL_STYLE, (style & !PBS_MARQUEE) as i32);
            SendMessageW(bar, PBM_SETRANGE32, 0, 100);
            SendMessageW(bar, PBM_SETPOS, percent.min(100) as usize, 0);
        } else if style & PBS_MARQUEE == 0 {
            SetWindowLongW(bar, GWL_STYLE, (style | PBS_MARQUEE) as i32);
            SendMessageW(bar, PBM_SETMARQUEE, 1, 30);
        } else {
            SendMessageW(bar, PBM_SETMARQUEE, 1, 30);
        }
        let text = wide(text);
        SetWindowTextW(GetDlgItem(hwnd, LABEL), text.as_ptr());
    }
}

/// Returns installation work, capped below completion until actual process success.
fn work_percent(current: u32, total: u32) -> Option<u32> {
    (total > 0).then(|| ((u64::from(current) * 100 / u64::from(total)).min(99)) as u32)
}

struct Process(HANDLE);
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Wait for a real dialog before letting the original game exit. Failure cancels arming.
pub fn launch(handoff: &freshen::Handoff) -> Result<(), Box<dyn std::error::Error>> {
    let parent = crate::update::servicing_root()?.join("progress");
    fs::create_dir_all(&parent)?;
    let directory = parent.join(format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir(&directory)?;
    let application = std::env::current_exe()?;
    let executable = directory.join("progress.exe");
    fs::copy(&application, &executable)?;
    fs::write(
        directory.join("application.json"),
        serde_json::to_vec(&application)?,
    )?;
    let mut child = Command::new(executable)
        .arg("--beeprs-update-progress")
        .arg(&directory)
        .arg(handoff.helper_pid().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait()?.is_some() {
            return Err(
                "The installation progress dialog could not start; the update was cancelled."
                    .into(),
            );
        }
        if directory.join("ready").is_file() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                "The installation progress dialog did not become ready; the update was cancelled."
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

struct Monitor {
    directory: PathBuf,
    root: PathBuf,
    application: PathBuf,
    helper: Process,
    finished: bool,
    success: bool,
    last: Option<(Option<u32>, String)>,
    error: Option<String>,
}

/// Dispatch before audio, the single-instance mutex, and normal startup acknowledgement.
pub fn run_monitor() -> Result<bool, Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--beeprs-update-progress")) {
        return Ok(false);
    }
    let directory = PathBuf::from(args.next().ok_or("missing progress context")?).canonicalize()?;
    let expected = crate::update::servicing_root()?
        .join("progress")
        .canonicalize()?;
    if directory.parent() != Some(expected.as_path())
        || std::env::current_exe()?.canonicalize()? != directory.join("progress.exe")
    {
        return Err("invalid progress context".into());
    }
    let pid: u32 = args
        .next()
        .ok_or("missing helper identity")?
        .to_str()
        .ok_or("invalid helper identity")?
        .parse()?;
    if args.next().is_some() || pid == 0 {
        return Err("invalid progress arguments".into());
    }
    let helper = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if helper.is_null() {
        return Err(io::Error::last_os_error().into());
    }
    let helper = Process(helper);
    let application: PathBuf =
        serde_json::from_slice(&fs::read(directory.join("application.json"))?)?;
    initialize()?;
    let mut state = Monitor {
        directory,
        root: crate::update::servicing_root()?,
        application,
        helper,
        finished: false,
        success: false,
        last: None,
        error: None,
    };
    let result = unsafe {
        DialogBoxParamW(
            GetModuleHandleW(std::ptr::null()),
            MONITOR_DIALOG as usize as *const u16,
            std::ptr::null_mut(),
            Some(monitor_proc),
            &mut state as *mut Monitor as LPARAM,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error().into());
    }
    if let Some(error) = state.error {
        return Err(error.into());
    }
    Ok(true)
}

unsafe extern "system" fn monitor_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    if message == WM_INITDIALOG {
        SetWindowLongPtrW(hwnd, DWLP_USER, lparam);
        let state = &mut *(lparam as *mut Monitor);
        show(
            hwnd,
            None,
            "Waiting for BeepRS to close. Installation cannot be cancelled after the game exits.",
        );
        if SetTimer(hwnd, 1, 200, None) == 0
            || fs::write(state.directory.join("ready"), b"ready").is_err()
        {
            state.error = Some("Could not initialize the progress dialog.".into());
            EndDialog(hwnd, 0);
        }
        return 1;
    }
    let pointer = GetWindowLongPtrW(hwnd, DWLP_USER) as *mut Monitor;
    if pointer.is_null() {
        return 0;
    }
    let state = &mut *pointer;
    match message {
        WM_TIMER => {
            poll(hwnd, state);
            1
        }
        WM_COMMAND if (wparam & 0xffff) as u16 == OPEN && state.success => {
            match Command::new(&state.application).spawn() {
                Ok(_) => {
                    EndDialog(hwnd, 1);
                }
                Err(error) => show(
                    hwnd,
                    Some(100),
                    &format!("Update installed, but the game could not be opened: {error}"),
                ),
            }
            1
        }
        WM_CLOSE | WM_COMMAND if message == WM_CLOSE || (wparam & 0xffff) as u16 == CANCEL => {
            if state.finished {
                KillTimer(hwnd, 1);
                EndDialog(hwnd, 0);
            }
            1
        }
        _ => 0,
    }
}

fn poll(hwnd: HWND, state: &mut Monitor) {
    if state.finished {
        return;
    }
    let helper_done = unsafe { WaitForSingleObject(state.helper.0, 0) } == WAIT_OBJECT_0;
    let snapshot = freshen::inno::status(&state.root);
    let (percent, text, terminal, success) = match snapshot {
        Ok(Some(status)) => {
            use freshen::inno::Phase;
            match status.phase {
                Phase::Completed if status.exit_code == Some(0) => (
                    Some(100),
                    "Update installed successfully.".into(),
                    true,
                    true,
                ),
                Phase::RebootRequired if status.exit_code.is_some() => (
                    Some(100),
                    "Update installed. Restart Windows before opening the game.".into(),
                    true,
                    false,
                ),
                Phase::Failed if status.exit_code.is_some() => (
                    Some(0),
                    format!(
                        "Installation failed (exit {}). {}",
                        status.exit_code.unwrap(),
                        status.detail
                    ),
                    true,
                    false,
                ),
                Phase::Installing => (
                    status
                        .progress
                        .and_then(|(current, total)| work_percent(current, total)),
                    if status.overdue {
                        "Installation is taking longer than expected. Waiting for the installer to finish.".into()
                    } else {
                        "Installing BeepRS...".into()
                    },
                    false,
                    false,
                ),
                Phase::Finalizing => (None, "Finalizing the installation...".into(), false, false),
                _ => (None, "Preparing the installation...".into(), false, false),
            }
        }
        Ok(None) => (None, "Waiting for the installer...".into(), false, false),
        Err(error) => (
            None,
            format!("Waiting for installation status: {error}"),
            false,
            false,
        ),
    };
    let (percent, text, terminal, success) = if helper_done && !terminal {
        (Some(0), "The updater stopped without a confirmed result. The outcome is unknown. Keep the update files for inspection; do not retry automatically.".into(), true, false)
    } else {
        (percent, text, terminal, success)
    };
    // Wait for helper teardown before enabling relaunch, so startup can safely read/clear state.
    state.finished = terminal && helper_done;
    state.success = success && state.finished;
    let display = (percent, text);
    if state.last.as_ref() != Some(&display) {
        show(hwnd, display.0, &display.1);
        state.last = Some(display);
    }
    unsafe {
        EnableWindow(GetDlgItem(hwnd, CANCEL.into()), state.finished.into());
        EnableWindow(GetDlgItem(hwnd, OPEN.into()), state.success.into());
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_work_does_not_claim_completion_or_overflow() {
        assert_eq!(work_percent(1, 4), Some(25));
        assert_eq!(work_percent(10, 10), Some(99));
        assert_eq!(work_percent(u32::MAX, 1), Some(99));
        assert_eq!(work_percent(0, 0), None);
    }

    #[test]
    fn native_dialog_and_installer_lifecycle() {
        initialize().unwrap();
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join(".freshen/transaction")).unwrap();
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, std::process::id()) };
        assert!(!process.is_null());
        let mut probe = Probe {
            monitor: Monitor {
                directory: directory.path().to_path_buf(),
                root: directory.path().to_path_buf(),
                application: std::env::current_exe().unwrap(),
                helper: Process(process),
                finished: false,
                success: false,
                last: None,
                error: None,
            },
            result: None,
        };
        let result = unsafe {
            DialogBoxParamW(
                GetModuleHandleW(std::ptr::null()),
                MONITOR_DIALOG as usize as *const u16,
                std::ptr::null_mut(),
                Some(probe_proc),
                &mut probe as *mut Probe as LPARAM,
            )
        };
        assert_ne!(result, -1, "{}", io::Error::last_os_error());
        if let Err(error) = probe.result.expect("dialog initialization") {
            std::panic::resume_unwind(error);
        }
    }

    struct Probe {
        monitor: Monitor,
        result: Option<std::thread::Result<()>>,
    }

    unsafe extern "system" fn probe_proc(
        hwnd: HWND,
        message: u32,
        _: WPARAM,
        lparam: LPARAM,
    ) -> isize {
        if message != WM_INITDIALOG {
            return 0;
        }
        let probe = &mut *(lparam as *mut Probe);
        // Never let a test assertion unwind through a system callback.
        probe.result = Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || {
                use windows_sys::Win32::UI::{
                    Controls::PBM_GETPOS,
                    Input::KeyboardAndMouse::IsWindowEnabled,
                    WindowsAndMessaging::{GetClassNameW, IsWindow},
                };
                let state = &mut probe.monitor;
                monitor_proc(hwnd, WM_INITDIALOG, 0, state as *mut Monitor as LPARAM);
                assert!(state.directory.join("ready").is_file());
                let mut class = [0u16; 80];
                let length = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
                assert_eq!(
                    String::from_utf16_lossy(&class[..length as usize]),
                    "#32770"
                );
                let bar = GetDlgItem(hwnd, BAR);
                let length = GetClassNameW(bar, class.as_mut_ptr(), class.len() as i32);
                assert_eq!(
                    String::from_utf16_lossy(&class[..length as usize]),
                    "msctls_progress32"
                );
                assert_eq!(IsWindowEnabled(GetDlgItem(hwnd, CANCEL.into())), 0);
                monitor_proc(hwnd, WM_CLOSE, 0, 0);
                assert_ne!(IsWindow(hwnd), 0);

                let path = state.root.join(".freshen/transaction/inno-status.json");
                let mut snapshot = freshen::inno::Status {
                    phase: freshen::inno::Phase::Installing,
                    progress: Some((25, 100)),
                    ..Default::default()
                };
                fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
                poll(hwnd, state);
                assert_eq!(SendMessageW(bar, PBM_GETPOS, 0, 0), 25);
                assert_eq!(GetWindowLongW(bar, GWL_STYLE) as u32 & PBS_MARQUEE, 0);
                snapshot.phase = freshen::inno::Phase::Finalizing;
                snapshot.progress = None;
                fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
                poll(hwnd, state);
                assert_ne!(GetWindowLongW(bar, GWL_STYLE) as u32 & PBS_MARQUEE, 0);

                snapshot.phase = freshen::inno::Phase::Completed;
                snapshot.exit_code = Some(0);
                fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
                poll(hwnd, state);
                assert!(!state.finished, "wait for helper teardown before relaunch");

                let mut child = Command::new("cmd.exe")
                    .args(["/C", "exit 0"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .unwrap();
                let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, child.id());
                assert!(!process.is_null());
                state.helper = Process(process);
                child.wait().unwrap();
                poll(hwnd, state);
                assert!(state.finished && state.success);
                assert_eq!(SendMessageW(bar, PBM_GETPOS, 0, 0), 100);
                assert_ne!(IsWindowEnabled(GetDlgItem(hwnd, OPEN.into())), 0);

                for phase in [
                    freshen::inno::Phase::Failed,
                    freshen::inno::Phase::RebootRequired,
                ] {
                    state.finished = false;
                    snapshot.phase = phase;
                    snapshot.exit_code = Some(if phase == freshen::inno::Phase::Failed {
                        7
                    } else {
                        3010
                    });
                    fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
                    poll(hwnd, state);
                    assert!(state.finished && !state.success);
                    assert_eq!(IsWindowEnabled(GetDlgItem(hwnd, OPEN.into())), 0);
                }
                state.finished = false;
                fs::remove_file(path).unwrap();
                poll(hwnd, state);
                assert!(state.finished && !state.success);
                assert!(state.last.as_ref().unwrap().1.contains("unknown"));
                assert_ne!(IsWindowEnabled(GetDlgItem(hwnd, CANCEL.into())), 0);
            },
        )));
        KillTimer(hwnd, 1);
        EndDialog(hwnd, 1);
        1
    }
}
