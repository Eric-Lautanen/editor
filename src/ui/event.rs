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

/// Where and how big the window is, in physical px, plus whether it is
/// maximized. The platform owns all of it (the OS moves and maximizes the
/// window); the app only *remembers* it, so the next session can reopen exactly
/// where this one was left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowGeometry {
    /// Client-area origin in screen coordinates.
    pub x: i32,
    pub y: i32,
    /// Client size (the canvas), physical px.
    pub width: u32,
    pub height: u32,
    /// Maximized *now*. A maximized window's rect is not where it goes back to,
    /// so the app keeps the last non-maximized one alongside this.
    pub maximized: bool,
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
    /// A *discrete* scroll step — a wheel notch, a tilt wheel — in physical px.
    /// Coarse by nature, so the app may glide to the new position instead of
    /// jumping; a precision touchpad's small deltas come through here too and
    /// are applied at once.
    Scroll {
        dx: f32,
        dy: f32,
    },
    /// Continuous manipulation: a finger (or pen) dragging the content. This
    /// tracks the input exactly — a glide in the way of a finger reads as lag
    /// — and on release hands over to [`Event::PanEnd`].
    Pan {
        dx: f32,
        dy: f32,
    },
    /// The finger came up, with its velocity across the last ~100 ms in client
    /// px/ms. A quick flick coasts from there; a release after the finger held
    /// still reports ~0 and stops dead.
    PanEnd {
        vx: f32,
        vy: f32,
    },
    /// A file opened by the host: drag-and-drop (anywhere on the window) or a
    /// macOS open-file message. The app opens it as a tab.
    FileOpen(std::path::PathBuf),
    /// Periodic host heartbeat (e.g. Win32 WM_TIMER) for drag auto-scroll.
    Tick,
    /// One animation frame, while the app has asked the host for them
    /// (`Host::set_animating`): the heartbeat is far too slow to look smooth,
    /// so the platform runs a faster clock for as long as something is moving.
    Frame,
    /// The OS light/dark preference changed (e.g. WM_SETTINGCHANGE).
    ThemeChanged {
        dark: bool,
    },
    /// The window moved, resized, or was maximized/restored. Sent on every such
    /// change, including the ones the OS makes on its own (a drag of the title
    /// bar, a maximize by the WM).
    WindowGeometry(WindowGeometry),
    CloseRequested,
}
