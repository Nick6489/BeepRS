//! Classic Win32 dialogs for the menu and the updater, and one plain window
//! for the game. The play window has no buttons. Space and Esc are the controls.
#![allow(unsafe_op_in_unsafe_fn)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use freshen::Cancellation;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CreateFontW, DEFAULT_GUI_FONT, DT_LEFT, DT_WORDBREAK,
    DeleteObject, DrawTextW, EndPaint, FW_NORMAL, GetStockObject, HFONT, PAINTSTRUCT, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::EM_SETLIMITTEXT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateDialogIndirectParamW, CreateWindowExW,
    DefWindowProcW, DestroyWindow, DialogBoxIndirectParamW, DispatchMessageW, EndDialog, GW_OWNER,
    GWLP_USERDATA, GetClientRect, GetDlgItem, GetDlgItemTextW, GetMessageW, GetWindow,
    GetWindowLongPtrW, IDC_ARROW, IsDialogMessageW, KillTimer, LB_ADDSTRING, LB_GETCURSEL,
    LB_RESETCONTENT, LBN_DBLCLK, LoadCursorW, MB_ICONINFORMATION, MB_OK, MSG, MessageBoxW,
    PostQuitMessage, RegisterClassW, SW_SHOW, SendMessageW, SetForegroundWindow, SetTimer,
    SetWindowLongPtrW, SetWindowTextW, ShowWindow, TranslateMessage, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WM_INITDIALOG, WM_KEYDOWN, WM_PAINT, WM_TIMER, WNDCLASSW, WS_CAPTION,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_VISIBLE,
};

use crate::audio::{Audio, Phase};
use crate::dialog::{
    self, ID_INSTALL, ID_LIST, ID_NAME, ID_NEW, ID_PLAY, ID_QUIT, ID_STATUS, ID_UPDATE,
};
use crate::save::{Game, Library};
use crate::update::{self, StartupReport};

const PLAY_TIMER: usize = 1;
const UPDATE_TIMER: usize = 2;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;

struct Shell {
    root: std::path::PathBuf,
    audio: Audio,
    library: Library,
    games: Vec<Game>,
    installed: Arc<Mutex<Option<freshen::Handoff>>>,
}

pub fn run(
    root: std::path::PathBuf,
    audio: Audio,
    library: Library,
    report: StartupReport,
) -> Result<Option<freshen::Handoff>, Box<dyn std::error::Error>> {
    if report.needs_attention {
        tell(std::ptr::null_mut(), &report.lines.join("\n"));
    }
    let games = library.list()?;
    let template = dialog::main_menu();
    let installed = Arc::new(Mutex::new(None));
    let shell = Box::new(Shell {
        root,
        audio,
        library,
        games,
        installed: installed.clone(),
    });
    let shell = Box::into_raw(shell);
    let instance = instance();
    let dialog = unsafe {
        CreateDialogIndirectParamW(
            instance,
            template.as_ptr().cast(),
            std::ptr::null_mut(),
            Some(main_proc),
            shell as LPARAM,
        )
    };
    if dialog.is_null() {
        let _ = unsafe { Box::from_raw(shell) };
        return Err("the main dialog could not be created".into());
    }
    unsafe {
        let mut message = MSG::default();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(dialog, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    Ok(installed
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take())
}

unsafe extern "system" fn main_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match message {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam);
            if let Some(shell) = shell_from(hwnd) {
                fill_list(hwnd, &shell.games);
            }
            1
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as u16;
            let notice = (wparam >> 16) as u16;
            if id == dialog::ID_LIST && notice == LBN_DBLCLK as u16 {
                play_selected(hwnd);
                return 1;
            }
            match id {
                ID_NEW => new_game(hwnd),
                ID_PLAY => play_selected(hwnd),
                ID_UPDATE => {
                    if let Some(shell) = shell_from(hwnd) {
                        let root = shell.root.clone();
                        if let Some(handoff) = update_dialog(hwnd, &root) {
                            *shell
                                .installed
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner()) = Some(handoff);
                            DestroyWindow(hwnd);
                        }
                    }
                }
                ID_QUIT | dialog::IDCANCEL => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            1
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            1
        }
        WM_DESTROY => {
            let shell = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Shell;
            if !shell.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(shell));
            }
            PostQuitMessage(0);
            0
        }
        _ => 0,
    }
}

fn new_game(hwnd: HWND) {
    let Some(name) = ask_name(hwnd) else {
        return;
    };
    let Some(shell) = shell_from(hwnd) else {
        return;
    };
    let game = match shell.library.create(&name) {
        Ok(game) => game,
        Err(error) => {
            tell(hwnd, &error.to_string());
            return;
        }
    };
    open_play(hwnd, game, true);
}

fn play_selected(hwnd: HWND) {
    let Some(shell) = shell_from(hwnd) else {
        return;
    };
    let list = unsafe { GetDlgItem(hwnd, i32::from(ID_LIST)) };
    let index = unsafe { SendMessageW(list, LB_GETCURSEL, 0, 0) };
    if index < 0 {
        tell(hwnd, "Choose a game first.");
        return;
    }
    let Some(game) = shell.games.get(index as usize).cloned() else {
        tell(hwnd, "Choose a game first.");
        return;
    };
    open_play(hwnd, game, false);
}

fn open_play(hwnd: HWND, game: Game, intro: bool) {
    let Some(shell) = shell_from(hwnd) else {
        return;
    };
    let audio = &shell.audio as *const Audio;
    let library = &shell.library as *const Library;
    unsafe { (*audio).start_game(intro) };
    let session = Box::new(Session {
        audio,
        library,
        game,
        font: unsafe {
            CreateFontW(
                22,
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                1,
                0,
                0,
                CLEARTYPE_QUALITY as u32,
                0,
                wide("Segoe UI").as_ptr(),
            )
        },
    });
    register_play_class();
    let title = wide(&window_title(
        &session.game,
        if intro { Phase::Intro } else { Phase::Playing },
    ));
    let class = wide("BeepRSPlay");
    let session_ptr = Box::into_raw(session);
    let play = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            560,
            260,
            hwnd,
            std::ptr::null_mut(),
            instance(),
            session_ptr as *mut _,
        )
    };
    if play.is_null() {
        unsafe { (*audio).stop() };
        drop(unsafe { Box::from_raw(session_ptr) });
        tell(hwnd, "The game window could not be created.");
        return;
    }
    unsafe {
        EnableWindow(hwnd, 0);
        ShowWindow(play, SW_SHOW);
        SetFocus(play);
        SetTimer(play, PLAY_TIMER, 200, None);
    }
}

unsafe extern "system" fn play_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => {
            let created =
                &*(lparam as *const windows_sys::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, created.lpCreateParams as isize);
            0
        }
        WM_KEYDOWN => {
            let repeated = (lparam & (1 << 30)) != 0;
            if !repeated {
                match wparam as u16 {
                    VK_SPACE => destroy_alien(hwnd),
                    VK_ESCAPE => {
                        DestroyWindow(hwnd);
                    }
                    _ => {}
                }
            }
            0
        }
        WM_TIMER => {
            if let Some(session) = session_from(hwnd) {
                let phase = unsafe { (*session.audio).phase() };
                let title = wide(&window_title(&session.game, phase));
                SetWindowTextW(hwnd, title.as_ptr());
                windows_sys::Win32::Graphics::Gdi::InvalidateRect(hwnd, std::ptr::null(), 1);
            }
            0
        }
        WM_PAINT => {
            paint_play(hwnd);
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, PLAY_TIMER);
            let session = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Session;
            if !session.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    (*(*session).audio).stop();
                    let owner = GetWindow(hwnd, GW_OWNER);
                    if !owner.is_null() {
                        EnableWindow(owner, 1);
                        if let Some(shell) = shell_from(owner)
                            && let Ok(games) = shell.library.list()
                        {
                            shell.games = games;
                            fill_list(owner, &shell.games);
                        }
                        SetForegroundWindow(owner);
                    }
                    if !(*session).font.is_null() {
                        DeleteObject((*session).font as _);
                    }
                    drop(Box::from_raw(session));
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn destroy_alien(hwnd: HWND) {
    let Some(session) = session_from(hwnd) else {
        return;
    };
    let destroyed = unsafe { (*session.audio).destroy_alien() };
    if !destroyed {
        return;
    }
    let next = session.game.aliens_destroyed.saturating_add(1);
    if let Err(error) = unsafe { (*session.library).store(&session.game, next) } {
        tell(hwnd, &error.to_string());
        return;
    }
    session.game.aliens_destroyed = next;
    let title = wide(&window_title(&session.game, Phase::Dying));
    unsafe {
        SetWindowTextW(hwnd, title.as_ptr());
        windows_sys::Win32::Graphics::Gdi::InvalidateRect(hwnd, std::ptr::null(), 1);
    }
}

fn paint_play(hwnd: HWND) {
    let Some(session) = session_from(hwnd) else {
        return;
    };
    let phase = unsafe { (*session.audio).phase() };
    let text = wide(&match phase {
        Phase::Intro => format!(
            "{}\n\nThe introduction is playing.\nSpace starts working when it finishes.\nEsc returns to the menu.",
            session.game.name
        ),
        Phase::Dying => format!(
            "{}\n{} aliens destroyed.\n\nSpace destroys the alien. There is no time limit.\nEsc returns to the menu.",
            session.game.name, session.game.aliens_destroyed
        ),
        Phase::Playing => format!(
            "{}\n{} aliens destroyed.\n\nSpace destroys the alien. There is no time limit.\nEsc returns to the menu.",
            session.game.name, session.game.aliens_destroyed
        ),
    });
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        let font = if session.font.is_null() {
            GetStockObject(DEFAULT_GUI_FONT)
        } else {
            session.font as _
        };
        SelectObject(dc, font);
        let mut rect = RECT::default();
        GetClientRect(hwnd, &mut rect);
        rect.left += 24;
        rect.top += 24;
        rect.right -= 24;
        rect.bottom -= 16;
        DrawTextW(dc, text.as_ptr(), -1, &mut rect, DT_LEFT | DT_WORDBREAK);
        EndPaint(hwnd, &paint);
    }
}

fn window_title(game: &Game, phase: Phase) -> String {
    match phase {
        Phase::Intro => format!("{} — introduction", game.name),
        Phase::Dying | Phase::Playing => {
            format!("{} — {} aliens", game.name, game.aliens_destroyed)
        }
    }
}

struct Session {
    audio: *const Audio,
    library: *const Library,
    game: Game,
    font: HFONT,
}

fn session_from(hwnd: HWND) -> Option<&'static mut Session> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Session;
    if pointer.is_null() {
        None
    } else {
        Some(unsafe { &mut *pointer })
    }
}

fn shell_from(hwnd: HWND) -> Option<&'static mut Shell> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Shell;
    if pointer.is_null() {
        None
    } else {
        Some(unsafe { &mut *pointer })
    }
}

fn fill_list(hwnd: HWND, games: &[Game]) {
    let list = unsafe { GetDlgItem(hwnd, i32::from(ID_LIST)) };
    unsafe { SendMessageW(list, LB_RESETCONTENT, 0, 0) };
    for game in games {
        let text = wide(&format!(
            "{} — {} aliens destroyed",
            game.name, game.aliens_destroyed
        ));
        unsafe { SendMessageW(list, LB_ADDSTRING, 0, text.as_ptr() as LPARAM) };
    }
}

fn ask_name(owner: HWND) -> Option<String> {
    let template = dialog::name_prompt();
    let mut chosen = None;
    let code = unsafe {
        DialogBoxIndirectParamW(
            instance(),
            template.as_ptr().cast(),
            owner,
            Some(name_proc),
            &mut chosen as *mut Option<String> as LPARAM,
        )
    };
    if code == 1 { chosen } else { None }
}

unsafe extern "system" fn name_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match message {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam);
            let edit = GetDlgItem(hwnd, i32::from(ID_NAME));
            SendMessageW(edit, EM_SETLIMITTEXT, 40, 0);
            SetFocus(edit);
            1
        }
        WM_COMMAND => {
            match (wparam & 0xFFFF) as u16 {
                dialog::IDOK => {
                    let mut buffer = [0u16; 64];
                    let edit = GetDlgItem(hwnd, i32::from(ID_NAME));
                    let count = GetDlgItemTextW(
                        hwnd,
                        i32::from(ID_NAME),
                        buffer.as_mut_ptr(),
                        buffer.len() as i32,
                    );
                    let _ = edit;
                    let name = String::from_utf16_lossy(&buffer[..count as usize]);
                    if name.trim().is_empty() {
                        tell(hwnd, "Enter a name.");
                        return 1;
                    }
                    let slot = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Option<String>;
                    if !slot.is_null() {
                        *slot = Some(name);
                    }
                    EndDialog(hwnd, 1);
                }
                dialog::IDCANCEL => {
                    EndDialog(hwnd, 0);
                }
                _ => {}
            }
            1
        }
        _ => 0,
    }
}

enum WorkerEvent {
    Status(String),
    Offer { version: String, notes: String },
    Armed(freshen::Handoff),
    Failed(String),
}

enum Decision {
    Install,
}

struct UpdateState {
    text: String,
    from_worker: Receiver<WorkerEvent>,
    to_worker: Sender<Decision>,
    cancel: Cancellation,
    alive: Arc<AtomicBool>,
    outcome: Arc<Mutex<Option<freshen::Handoff>>>,
    offered: bool,
}

fn update_dialog(owner: HWND, root: &std::path::Path) -> Option<freshen::Handoff> {
    let loaded = match update::load_source(root) {
        Ok(loaded) => loaded,
        Err(error) => {
            tell(
                owner,
                &format!("{error}\r\n\r\n{}", update::missing_source_message(root)),
            );
            return None;
        }
    };
    let (event_tx, event_rx) = mpsc::channel();
    let (decision_tx, decision_rx) = mpsc::channel();
    let cancel = Cancellation::default();
    let alive = Arc::new(AtomicBool::new(true));
    let worker_cancel = cancel.clone();
    let worker_alive = alive.clone();
    let worker_root = root.to_path_buf();
    std::thread::spawn(move || {
        update_worker(
            worker_root,
            loaded,
            worker_cancel,
            worker_alive,
            event_tx,
            decision_rx,
        );
    });
    let outcome = Arc::new(Mutex::new(None));
    let state = Box::new(UpdateState {
        text: update::identity_line(),
        from_worker: event_rx,
        to_worker: decision_tx,
        cancel,
        alive,
        outcome: outcome.clone(),
        offered: false,
    });
    let template = dialog::update_prompt();
    unsafe {
        DialogBoxIndirectParamW(
            instance(),
            template.as_ptr().cast(),
            owner,
            Some(update_proc),
            Box::into_raw(state) as LPARAM,
        );
    }
    outcome
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take()
}

fn update_worker(
    root: std::path::PathBuf,
    loaded: update::LoadedSource,
    cancel: Cancellation,
    alive: Arc<AtomicBool>,
    events: Sender<WorkerEvent>,
    decisions: Receiver<Decision>,
) {
    let mut report = |line: String| {
        let _ = events.send(WorkerEvent::Status(line));
    };
    let offer = match update::find_release(&loaded, &cancel, &mut report) {
        Ok(Some(offer)) => offer,
        Ok(None) => {
            let _ = events.send(WorkerEvent::Status(
                "No newer signed release is available.".into(),
            ));
            return;
        }
        Err(error) => {
            let _ = events.send(WorkerEvent::Failed(error.to_string()));
            return;
        }
    };
    let _ = events.send(WorkerEvent::Offer {
        version: offer.version.clone(),
        notes: offer.notes.clone(),
    });
    if !matches!(decisions.recv(), Ok(Decision::Install)) || !alive.load(Ordering::Acquire) {
        return;
    }
    let prepared = match update::download_release(offer, &cancel, &mut report) {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = events.send(WorkerEvent::Failed(error.to_string()));
            return;
        }
    };
    if !alive.load(Ordering::Acquire) || cancel.check().is_err() {
        return;
    }
    match update::install_release(&root, prepared) {
        Ok(handoff) => {
            let _ = events.send(WorkerEvent::Armed(handoff));
        }
        Err(error) => {
            let _ = events.send(WorkerEvent::Failed(error.to_string()));
        }
    }
}

unsafe extern "system" fn update_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    match message {
        WM_INITDIALOG => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam);
            let state = &*(lparam as *const UpdateState);
            set_status(hwnd, &state.text);
            EnableWindow(GetDlgItem(hwnd, i32::from(ID_INSTALL)), 0);
            SetTimer(hwnd, UPDATE_TIMER, 100, None);
            1
        }
        WM_TIMER => {
            pump_update(hwnd);
            1
        }
        WM_COMMAND => {
            match (wparam & 0xFFFF) as u16 {
                ID_INSTALL => {
                    if let Some(state) = update_from(hwnd)
                        && state.offered
                    {
                        state.offered = false;
                        EnableWindow(GetDlgItem(hwnd, i32::from(ID_INSTALL)), 0);
                        let _ = state.to_worker.send(Decision::Install);
                        set_status(hwnd, "Downloading the release.");
                    }
                }
                dialog::IDCANCEL => {
                    close_update(hwnd);
                }
                _ => {}
            }
            1
        }
        WM_CLOSE => {
            close_update(hwnd);
            1
        }
        WM_DESTROY => {
            KillTimer(hwnd, UPDATE_TIMER);
            let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut UpdateState;
            if !state.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    (*state).alive.store(false, Ordering::Release);
                    (*state).cancel.cancel();
                    drop(Box::from_raw(state));
                }
            }
            0
        }
        _ => 0,
    }
}

fn pump_update(hwnd: HWND) {
    let Some(state) = update_from(hwnd) else {
        return;
    };
    while let Ok(event) = state.from_worker.try_recv() {
        match event {
            WorkerEvent::Status(line) => {
                state.text.push_str("\r\n");
                state.text.push_str(&line);
                set_status(hwnd, &state.text);
            }
            WorkerEvent::Offer { version, notes } => {
                state.offered = true;
                state
                    .text
                    .push_str(&format!("\r\nVersion {version} is available."));
                if !notes.is_empty() {
                    state.text.push_str("\r\n");
                    state.text.push_str(&notes);
                }
                state.text.push_str(
                    "\r\n\r\nInstall replaces the program and the sounds. Saved games stay.",
                );
                set_status(hwnd, &state.text);
                unsafe { EnableWindow(GetDlgItem(hwnd, i32::from(ID_INSTALL)), 1) };
            }
            WorkerEvent::Armed(handoff) => {
                *state
                    .outcome
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = Some(handoff);
                unsafe { EndDialog(hwnd, 1) };
                return;
            }
            WorkerEvent::Failed(error) => {
                state.offered = false;
                state.text.push_str("\r\n");
                state.text.push_str(&error);
                set_status(hwnd, &state.text);
                unsafe { EnableWindow(GetDlgItem(hwnd, i32::from(ID_INSTALL)), 0) };
            }
        }
    }
}

fn close_update(hwnd: HWND) {
    if let Some(state) = update_from(hwnd) {
        state.alive.store(false, Ordering::Release);
        state.cancel.cancel();
    }
    unsafe { EndDialog(hwnd, 0) };
}

fn update_from(hwnd: HWND) -> Option<&'static mut UpdateState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut UpdateState;
    if pointer.is_null() {
        None
    } else {
        Some(unsafe { &mut *pointer })
    }
}

fn set_status(hwnd: HWND, text: &str) {
    let status = unsafe { GetDlgItem(hwnd, i32::from(ID_STATUS)) };
    let wide = wide(text);
    unsafe { SetWindowTextW(status, wide.as_ptr()) };
}

fn register_play_class() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let class_name = Box::leak(wide("BeepRSPlay").into_boxed_slice());
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(play_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance(),
            hIcon: std::ptr::null_mut(),
            hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
            hbrBackground: (5 + 1) as _,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        unsafe { RegisterClassW(&class) };
    });
}

fn instance() -> windows_sys::Win32::Foundation::HINSTANCE {
    unsafe { GetModuleHandleW(std::ptr::null()) }
}

fn tell(owner: HWND, text: &str) {
    let text = wide(text);
    let title = wide("BeepRS");
    unsafe {
        MessageBoxW(
            owner,
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        )
    };
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
