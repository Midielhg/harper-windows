//! Watches for the shortcut that applies every suggestion in the focused field at once.
//!
//! The overlay never takes keyboard focus, so the key press has to be caught before it reaches the
//! app the user is typing in. It is also swallowed, but only while Harper is visibly offering
//! something to accept: Tab has to keep indenting, moving between fields, and completing in a
//! terminal every other moment.

#[cfg(windows)]
mod implementation {
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_TAB;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, HHOOK, KBDLLHOOKSTRUCT, SetWindowsHookExW, UnhookWindowsHookEx,
        WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
    };

    /// Whether Harper is offering something the shortcut could accept. The hook runs on this
    /// thread but outside any of our own calls, so the state it needs has to live somewhere it can
    /// reach without a receiver.
    static ARMED: AtomicBool = AtomicBool::new(false);
    /// Set by the hook when the shortcut is pressed, and cleared by the window manager when it acts.
    static REQUESTED: AtomicBool = AtomicBool::new(false);

    pub struct FixAllHotkey {
        hook: Option<HHOOK>,
    }

    impl FixAllHotkey {
        /// Installs the hook on the calling thread, which must be the one running the event loop
        /// because a low-level keyboard hook is only delivered to a thread that pumps messages.
        pub fn install() -> Self {
            let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) }
                .inspect_err(|error| {
                    eprintln!("Unable to watch for the fix-all shortcut: {error}");
                })
                .ok();

            Self { hook }
        }

        pub fn set_armed(&self, armed: bool) {
            ARMED.store(armed, Ordering::Relaxed);
        }

        /// Returns whether the shortcut was pressed since the last call.
        pub fn take_request(&self) -> bool {
            REQUESTED.swap(false, Ordering::Relaxed)
        }
    }

    impl Drop for FixAllHotkey {
        fn drop(&mut self) {
            if let Some(hook) = self.hook.take() {
                let _ = unsafe { UnhookWindowsHookEx(hook) };
            }
        }
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // A negative code means the event is not ours to inspect, only to pass along.
        if code < 0 {
            return unsafe { CallNextHookEx(None, code, wparam, lparam) };
        }

        let is_key_down = wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN;
        let key = unsafe { (lparam.0 as *const KBDLLHOOKSTRUCT).as_ref() }
            .map(|event| event.vkCode)
            .unwrap_or_default();

        if is_key_down && key == VK_TAB.0 as u32 && ARMED.load(Ordering::Relaxed) {
            REQUESTED.store(true, Ordering::Relaxed);

            // Returning a non-zero value keeps the key from reaching the focused app, so the Tab
            // that asked for the fixes does not also land in the text.
            return LRESULT(1);
        }

        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

#[cfg(not(windows))]
mod implementation {
    /// Other platforms have no shortcut yet, so the popup is dismissed through its own controls.
    pub struct FixAllHotkey;

    impl FixAllHotkey {
        pub fn install() -> Self {
            Self
        }

        pub fn set_armed(&self, _armed: bool) {}

        pub fn take_request(&self) -> bool {
            false
        }
    }
}

pub use implementation::FixAllHotkey;
