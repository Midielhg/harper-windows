//! Watches for the shortcut that applies every suggestion in the focused field at once.
//!
//! The overlay never takes keyboard focus, so the key press has to be caught before it reaches the
//! app the user is typing in. It is also swallowed, but only while Harper is visibly offering
//! something to accept: Tab has to keep indenting, moving between fields, and completing in a
//! terminal every other moment.

#[cfg(windows)]
mod implementation {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::channel;
    use std::thread::JoinHandle;

    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_TAB;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, MSG, PostThreadMessageW, SetWindowsHookExW,
        UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
    };

    /// Whether Harper is offering something the shortcut could accept. The hook runs on this
    /// thread but outside any of our own calls, so the state it needs has to live somewhere it can
    /// reach without a receiver.
    static ARMED: AtomicBool = AtomicBool::new(false);
    /// Set by the hook when the shortcut is pressed, and cleared by the window manager when it acts.
    static REQUESTED: AtomicBool = AtomicBool::new(false);

    pub struct FixAllHotkey {
        watcher: Option<(u32, JoinHandle<()>)>,
    }

    impl FixAllHotkey {
        /// Installs the hook on a thread of its own, which does nothing but wait for keys.
        ///
        /// A low-level keyboard hook is only delivered to a thread that is pumping messages, and
        /// every key pressed anywhere on the machine waits on it: Windows holds the key until the
        /// hook answers or `LowLevelHooksTimeout` — 300ms by default — runs out. Installed on the
        /// event loop, as it first was, the hook went unanswered for as long as that loop spent
        /// asking the focused app where its text is, which is up to a quarter of a second at a
        /// time. The cost of that landed on every keystroke in every app, Harper's own window or
        /// not, and grew with the length of what was being written. On a thread that only waits,
        /// the hook answers at once however busy the rest of Harper is.
        pub fn install() -> Self {
            let (report_thread, thread_id) = channel();

            let watcher = std::thread::Builder::new()
                .name("fix-all-hotkey".to_string())
                .spawn(move || {
                    let hook =
                        unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) }
                            .inspect_err(|error| {
                                eprintln!("Unable to watch for the fix-all shortcut: {error}");
                            })
                            .ok();

                    // Sent after the hook is installed but whether or not it worked, so install()
                    // never waits for a thread that is already giving up.
                    let _ = report_thread.send(unsafe { GetCurrentThreadId() });

                    let Some(hook) = hook else {
                        return;
                    };

                    // Nothing is dispatched: the hook is called by the system as the thread waits
                    // here, and this thread owns no windows of its own. The loop ends on the
                    // WM_QUIT posted by `Drop`, and on the -1 that a failed `GetMessageW` returns.
                    let mut message = MSG::default();

                    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {}

                    let _ = unsafe { UnhookWindowsHookEx(hook) };
                })
                .inspect_err(|error| {
                    eprintln!("Unable to start the fix-all shortcut watcher: {error}");
                })
                .ok();

            let watcher = watcher.and_then(|handle| Some((thread_id.recv().ok()?, handle)));

            Self { watcher }
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
            let Some((thread_id, handle)) = self.watcher.take() else {
                return;
            };

            // The hook has to be removed by the thread that installed it, so the thread is asked
            // to finish and waited for rather than simply dropped.
            let _ = unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            let _ = handle.join();
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
