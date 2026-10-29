//! Puts text on the clipboard for long enough to paste it, then puts back what was there.
//!
//! Applying a fix means getting the replacement into the field. Typing it sends one keystroke per
//! character, and measurement on a real session showed each one costing hundreds of milliseconds:
//! a three-character fix took about two seconds and a ten-character fix nearly five. The cost sat
//! in the single `SendInput` call, and it grew with the number of characters rather than the size
//! of the text, so the thing to cut is the count of keystrokes, not their contents.
//!
//! A paste is one keystroke no matter how long the replacement is.

use std::thread::sleep;
use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// How long to keep trying to open the clipboard.
///
/// Only one process may hold it at a time, and on a busy desktop something else often has it for a
/// moment. Giving up immediately would drop the fix, so this waits out the usual contention.
const OPEN_TIMEOUT: Duration = Duration::from_millis(300);
/// How long to wait between attempts to open the clipboard.
const OPEN_RETRY: Duration = Duration::from_millis(10);

/// Holds the clipboard open for one borrow, closing it however the borrow ends.
struct OpenClipboardGuard;

impl OpenClipboardGuard {
    /// Opens the clipboard, waiting for whoever else has it to let go.
    fn open() -> Option<Self> {
        let deadline = std::time::Instant::now() + OPEN_TIMEOUT;

        loop {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Some(Self);
            }

            if std::time::Instant::now() >= deadline {
                return None;
            }

            sleep(OPEN_RETRY);
        }
    }
}

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

/// Puts `text` on the clipboard, putting back the text that was there when this is dropped.
///
/// The replacement has to stay on the clipboard until the app being corrected has actually read
/// it, which is why this is a guard rather than a pair of calls: the caller holds it until the
/// pasted text has shown up in the field.
///
/// Only text is carried over. Anything else that was on the clipboard — a copied image, a file,
/// something an app offers in its own format — does not survive, because putting a new entry on
/// the clipboard clears what was there and this cannot reproduce a format it does not understand.
/// That is the cost of the paste, and it is why the caller only reaches for one when typing the
/// replacement would be slow enough to notice.
#[must_use = "the clipboard is put back when this is dropped, so it has to outlive the paste"]
pub struct ClipboardText {
    previous: Option<String>,
}

impl ClipboardText {
    /// Returns `None` if the clipboard could not be taken, in which case nothing was changed.
    pub fn set(text: &str) -> Option<Self> {
        let guard = OpenClipboardGuard::open()?;

        let previous = read_text();

        if !write_text(text) {
            // The clipboard was emptied before the write failed, so there is nothing to restore
            // onto and saying so is better than leaving a half-done state to a later drop.
            drop(guard);
            return None;
        }

        drop(guard);

        Some(Self { previous })
    }
}

impl Drop for ClipboardText {
    fn drop(&mut self) {
        let Some(guard) = OpenClipboardGuard::open() else {
            return;
        };

        match &self.previous {
            Some(previous) => {
                write_text(previous);
            }
            // Nothing was there before, so leaving the replacement behind would be adding
            // something the person never copied.
            None => {
                let _ = unsafe { EmptyClipboard() };
            }
        }

        drop(guard);
    }
}

/// The clipboard's text, if it holds any. The clipboard must already be open.
fn read_text() -> Option<String> {
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT.0 as u32) }.ok()?;

    if handle.is_invalid() {
        return None;
    }

    let memory = HGLOBAL(handle.0);
    let pointer = unsafe { GlobalLock(memory) } as *const u16;

    if pointer.is_null() {
        return None;
    }

    // The clipboard's text is terminated by a zero, which is not part of the text itself.
    let mut length = 0;

    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }

    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) });

    let _ = unsafe { GlobalUnlock(memory) };

    Some(text)
}

/// Replaces the clipboard's contents with `text`. The clipboard must already be open.
fn write_text(text: &str) -> bool {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = std::mem::size_of_val(units.as_slice());

    let Ok(memory) = (unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) }) else {
        return false;
    };

    let pointer = unsafe { GlobalLock(memory) } as *mut u16;

    if pointer.is_null() {
        return false;
    }

    unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len()) };
    let _ = unsafe { GlobalUnlock(memory) };

    if unsafe { EmptyClipboard() }.is_err() {
        return false;
    }

    // Windows owns the memory once this succeeds, so it must not be freed here. When it fails,
    // ownership stays here and the block is released instead.
    if unsafe { SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(memory.0))) }.is_err() {
        let _ = unsafe { GlobalFree(Some(memory)) };
        return false;
    }

    true
}
