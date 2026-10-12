/// Modifier keys held during a key or mouse event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Tab,
    Escape,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Mouse cursor shapes the app can ask for (I-beam over text, hand over
/// clickable things). Platforms map these to their system cursors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Arrow,
    IBeam,
    Hand,
}

/// Platform-neutral events delivered by `platform::run`.
/// Grows per milestone: macOS open-file arrives with M9.
#[derive(Clone, Debug)]
pub enum Event {
    Resized {
        width: usize,
        height: usize,
        scale: f32,
    },
    KeyDown {
        key: Key,
        mods: Modifiers,
    },
    /// Text input (after IME/layout), distinct from KeyDown.
    Text(String),
    MouseDown {
        x: f32,
        y: f32,
        button: MouseButton,
    },
    MouseUp {
        x: f32,
        y: f32,
        button: MouseButton,
    },
    MouseMove {
        x: f32,
        y: f32,
    },
    Scroll {
        dx: f32,
        dy: f32,
    },
    /// A file opened by the host: drag-and-drop (anywhere on the window) or a
    /// macOS open-file message. The app opens it as a tab.
    FileOpen(std::path::PathBuf),
    /// Periodic host heartbeat (e.g. Win32 WM_TIMER) for drag auto-scroll.
    Tick,
    /// The OS light/dark preference changed (e.g. WM_SETTINGCHANGE).
    ThemeChanged {
        dark: bool,
    },
    CloseRequested,
}
