//! In-memory dialog templates. This is the resource-script half of a Petzold
//! dialog, written as bytes because the program does not ship a compiled `.res`.

const WS_POPUP: u32 = 0x8000_0000;
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const DS_SETFONT: u32 = 0x0040;
const DS_MODALFRAME: u32 = 0x0080;
const DS_CENTER: u32 = 0x0800;
const DS_SETFOREGROUND: u32 = 0x0200;

const BUTTON: u16 = 0x0080;
const EDIT: u16 = 0x0081;
const STATIC: u16 = 0x0082;
const LISTBOX: u16 = 0x0083;

const BS_DEFPUSHBUTTON: u32 = 0x0001;
const ES_AUTOHSCROLL: u32 = 0x0080;
const ES_MULTILINE: u32 = 0x0004;
const ES_READONLY: u32 = 0x0800;
const ES_AUTOVSCROLL: u32 = 0x0040;
const LBS_NOTIFY: u32 = 0x0001;
const LBS_NOINTEGRALHEIGHT: u32 = 0x0100;
const SS_NOPREFIX: u32 = 0x0080;
const SS_EDITCONTROL: u32 = 0x2000;

pub const ID_LIST: u16 = 100;
pub const ID_NEW: u16 = 101;
pub const ID_PLAY: u16 = 102;
pub const ID_UPDATE: u16 = 103;
pub const ID_QUIT: u16 = 104;
pub const ID_INFO: u16 = 105;
pub const ID_NAME: u16 = 110;
pub const ID_STATUS: u16 = 120;
pub const ID_INSTALL: u16 = 121;
pub const IDOK: u16 = 1;
pub const IDCANCEL: u16 = 2;

struct Item {
    style: u32,
    x: i16,
    y: i16,
    cx: i16,
    cy: i16,
    id: u16,
    class: u16,
    title: String,
}

struct Template {
    title: String,
    cx: i16,
    cy: i16,
    items: Vec<Item>,
}

impl Template {
    fn dialog(title: &str, cx: i16, cy: i16) -> Self {
        Self {
            title: title.to_string(),
            cx,
            cy,
            items: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn item(
        &mut self,
        class: u16,
        title: &str,
        id: u16,
        x: i16,
        y: i16,
        cx: i16,
        cy: i16,
        style: u32,
    ) {
        self.items.push(Item {
            style: WS_CHILD | WS_VISIBLE | style,
            x,
            y,
            cx,
            cy,
            id,
            class,
            title: title.to_string(),
        });
    }

    fn bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let style = DS_SETFONT
            | DS_MODALFRAME
            | DS_CENTER
            | DS_SETFOREGROUND
            | WS_POPUP
            | WS_CAPTION
            | WS_SYSMENU
            | WS_VISIBLE;
        write_u32(&mut bytes, style);
        write_u32(&mut bytes, 0);
        write_u16(&mut bytes, self.items.len() as u16);
        write_i16(&mut bytes, 0);
        write_i16(&mut bytes, 0);
        write_i16(&mut bytes, self.cx);
        write_i16(&mut bytes, self.cy);
        write_u16(&mut bytes, 0);
        write_u16(&mut bytes, 0);
        write_wide(&mut bytes, &self.title);
        write_u16(&mut bytes, 9);
        write_wide(&mut bytes, "MS Shell Dlg");
        for item in &self.items {
            align4(&mut bytes);
            write_u32(&mut bytes, item.style);
            write_u32(&mut bytes, 0);
            write_i16(&mut bytes, item.x);
            write_i16(&mut bytes, item.y);
            write_i16(&mut bytes, item.cx);
            write_i16(&mut bytes, item.cy);
            write_u16(&mut bytes, item.id);
            write_u16(&mut bytes, 0xFFFF);
            write_u16(&mut bytes, item.class);
            write_wide(&mut bytes, &item.title);
            write_u16(&mut bytes, 0);
        }
        bytes
    }
}

pub fn main_menu(summary: &str) -> Vec<u8> {
    let mut dialog = Template::dialog("BeepRS", 312, 220);
    dialog.item(
        STATIC,
        summary,
        ID_INFO,
        8,
        6,
        296,
        40,
        SS_NOPREFIX | SS_EDITCONTROL,
    );
    dialog.item(
        LISTBOX,
        "",
        ID_LIST,
        8,
        50,
        296,
        96,
        WS_TABSTOP | WS_BORDER | WS_VSCROLL | LBS_NOTIFY | LBS_NOINTEGRALHEIGHT,
    );
    dialog.item(BUTTON, "&New", ID_NEW, 8, 154, 70, 16, WS_TABSTOP);
    dialog.item(
        BUTTON,
        "&Play",
        ID_PLAY,
        84,
        154,
        70,
        16,
        WS_TABSTOP | BS_DEFPUSHBUTTON,
    );
    dialog.item(BUTTON, "&Update", ID_UPDATE, 160, 154, 70, 16, WS_TABSTOP);
    dialog.item(BUTTON, "&Quit", ID_QUIT, 236, 154, 68, 16, WS_TABSTOP);
    dialog.item(
        STATIC,
        "Space destroys the alien. Esc leaves the game.",
        106,
        8,
        178,
        296,
        16,
        SS_NOPREFIX,
    );
    dialog.bytes()
}

pub fn name_prompt() -> Vec<u8> {
    let mut dialog = Template::dialog("New game", 228, 78);
    dialog.item(STATIC, "Name this game.", 111, 8, 8, 210, 10, SS_NOPREFIX);
    dialog.item(
        EDIT,
        "",
        ID_NAME,
        8,
        22,
        210,
        14,
        WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
    );
    dialog.item(
        BUTTON,
        "OK",
        IDOK,
        8,
        46,
        60,
        16,
        WS_TABSTOP | BS_DEFPUSHBUTTON,
    );
    dialog.item(BUTTON, "Cancel", IDCANCEL, 74, 46, 60, 16, WS_TABSTOP);
    dialog.bytes()
}

pub fn update_prompt() -> Vec<u8> {
    let mut dialog = Template::dialog("Update BeepRS", 340, 210);
    dialog.item(
        EDIT,
        "",
        ID_STATUS,
        8,
        8,
        324,
        148,
        WS_TABSTOP
            | WS_BORDER
            | WS_VSCROLL
            | ES_MULTILINE
            | ES_READONLY
            | ES_AUTOVSCROLL
            | SS_NOPREFIX,
    );
    dialog.item(
        BUTTON,
        "&Install and quit",
        ID_INSTALL,
        8,
        166,
        110,
        16,
        WS_TABSTOP,
    );
    dialog.item(BUTTON, "&Close", IDCANCEL, 126, 166, 70, 16, WS_TABSTOP);
    dialog.bytes()
}

fn align4(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

fn write_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_i16(bytes: &mut Vec<u8>, value: i16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_wide(bytes: &mut Vec<u8>, text: &str) {
    for unit in text.encode_utf16() {
        write_u16(bytes, unit);
    }
    write_u16(bytes, 0);
}
