use crate::windows_broker::automation_service::AutomationService;
use crate::{
    os_broker::{AccessibilityPermissionStatus, AppSearchResult, OsBroker},
    rect::ActionableLint,
};
use cached::cached;
use egui::Pos2;
use harper_core::{Span, linting::Lint, linting::Suggestion};
use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use std::process::Command;
use std::time::{Duration, Instant};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use windows::Win32::Foundation::{CloseHandle, HWND, POINT};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    GUI_INMOVESIZE, GUITHREADINFO, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo,
    GetWindowThreadProcessId,
};
use windows::core::{PWSTR, Result as WindowsResult};
use wintheon::file::{IconSize, Priority};
use wintheon::gather::Gatherer;
mod automation_service;

/// How long the text has to stay unchanged before Harper reads the screen positions of its lints.
///
/// Those positions come from the accessibility API, which an app answers on the same thread it
/// handles typing on. Asking after every keystroke makes the app alternate between serving Harper
/// and taking the next key, which is felt as lag while typing. Waiting for a short pause costs
/// nothing in practice: it is roughly the gap after finishing a word.
const TEXT_SETTLE: Duration = Duration::from_millis(350);

/// The shortest gap between two reads of the focused app.
///
/// Reading is a cross-process accessibility call that the target app answers on the thread it also
/// takes keystrokes on, so the overlay asking as fast as it can is felt as the app typing slowly.
/// The loop used to be paced only by how long each read took, which meant the busier the app was,
/// the harder Harper leaned on it. A fixed gap puts a ceiling on that, and six reads a second is
/// still far quicker than anyone notices.
const READ_INTERVAL: Duration = Duration::from_millis(160);

/// How much of the target app's time Harper is willing to take, as a divisor.
///
/// A read is served by the app's own UI thread, so a read that takes 100ms is 100ms that thread
/// cannot spend on the keystroke waiting behind it. Spacing the next read at this multiple of the
/// last one's cost keeps Harper to roughly a quarter of that thread no matter how slow the app is,
/// which is why a heavy editor no longer gets hammered the hardest.
const READ_DUTY_DIVISOR: u32 = 4;

pub struct WindowsBroker {
    service: Arc<Mutex<AutomationService>>,
    is_integration_enabled: Box<dyn FnMut(&str) -> bool + Send + Sync>,
    /// The text seen at the last read, and when it last differed from the read before it.
    last_text: Option<String>,
    last_text_change: Option<Instant>,
    /// When the last read of the focused app started, used to keep the reads spaced out.
    last_read: Option<Instant>,
    /// How long that read took, which sets how long to leave the app alone afterwards.
    last_read_cost: Duration,
    /// The text that was last linted, and what linting it produced.
    ///
    /// Lints depend on nothing but the text, so re-running them on text that has not changed is
    /// pure waste — and it was running several times a second for as long as a window stayed open.
    cached_lints: Option<(String, BTreeMap<String, Vec<Lint>>)>,
}

impl WindowsBroker {
    /// Creates a broker with an app policy that may register newly encountered executable paths.
    /// The policy is called before reading the app's text and may change as settings are refreshed.
    pub fn new(is_integration_enabled: impl FnMut(&str) -> bool + Send + Sync + 'static) -> Self {
        Self {
            service: Arc::new(Mutex::new(AutomationService::create_and_start())),
            is_integration_enabled: Box::new(is_integration_enabled),
            last_text: None,
            last_text_change: None,
            last_read: None,
            last_read_cost: Duration::ZERO,
            cached_lints: None,
        }
    }

    /// Whether the text has been the same for long enough to be worth measuring again.
    fn text_has_settled(&mut self, text: &str) -> bool {
        if self.last_text.as_deref() != Some(text) {
            self.last_text = Some(text.to_string());
            self.last_text_change = Some(Instant::now());

            return false;
        }

        self.last_text_change
            .is_none_or(|changed_at| changed_at.elapsed() >= TEXT_SETTLE)
    }

    pub fn should_lint_focused_window(&mut self) -> Option<bool> {
        let mut service = self.service.lock().ok()?;
        let focused_window = service.resolve_focused_window()?;
        let path = get_window_path(focused_window).ok()?;
        drop(service);

        if !(self.is_integration_enabled)(&path.to_string_lossy()) {
            return Some(false);
        }

        match window_is_moving(HWND(focused_window as *mut c_void)) {
            Ok(is_moving) => Some(!is_moving),
            Err(error) => {
                eprintln!(
                    "Unable to determine whether the focused Windows window is moving: {error}"
                );
                None
            }
        }
    }
}

impl OsBroker for WindowsBroker {
    fn is_harper_desktop(app_id: &str) -> bool {
        let Ok(executable) = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .inspect_err(|error| eprintln!("failed to identify Harper executable: {error}"))
        else {
            return false;
        };
        std::fs::canonicalize(app_id).is_ok_and(|path| {
            path.to_string_lossy()
                .eq_ignore_ascii_case(&executable.to_string_lossy())
        })
    }

    fn get_boxes(
        &mut self,
        lint_text: &mut dyn FnMut(&str) -> BTreeMap<String, Vec<Lint>>,
    ) -> Option<Vec<ActionableLint>> {
        let interval = READ_INTERVAL.max(self.last_read_cost * READ_DUTY_DIVISOR);

        if self
            .last_read
            .is_some_and(|last_read| last_read.elapsed() < interval)
        {
            return None;
        }

        let read_started = Instant::now();
        self.last_read = Some(read_started);

        let boxes = self.read_boxes(lint_text);
        self.last_read_cost = read_started.elapsed();

        boxes
    }

    fn cursor_position(&self) -> Option<Pos2> {
        let mut point = POINT::default();

        unsafe {
            GetCursorPos(&mut point).unwrap();
        }

        let monitor_scale = get_focused_monitor_scale();

        let pos = Pos2::new(
            point.x as f32 / monitor_scale as f32,
            point.y as f32 / monitor_scale as f32,
        );

        Some(pos)
    }

    fn apply_all_suggestions(&mut self, source_text: &str, edits: &[(Span<char>, Suggestion)]) {
        let Ok(mut service) = self.service.lock() else {
            return;
        };

        service.apply_suggestions(source_text.to_string(), edits.to_vec());
    }

    fn pointer_pressed(&self) -> bool {
        // The high bit is set while the button is down. The low bit, which reports whether the
        // button was pressed since the last call, is deliberately ignored: this is polled every
        // frame and only the current state matters.
        let state = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) };

        state as u16 & 0x8000 != 0
    }

    fn accessibility_permission_status(&self) -> AccessibilityPermissionStatus {
        AccessibilityPermissionStatus::Granted
    }

    fn request_accessibility_permission(&self) -> AccessibilityPermissionStatus {
        AccessibilityPermissionStatus::Granted
    }

    fn integration_display_name(&self, bundle_id: &str) -> String {
        if let Some(entry) = look_up_application(bundle_id) {
            return entry.display_name;
        }

        // An app missing from the installed-app catalog still has a name worth showing. Fall back
        // to the executable's own name rather than its whole path, which is too long to read and
        // is available through the integration's About row.
        PathBuf::from(bundle_id)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| bundle_id.to_string())
    }

    fn installed_application_bundle_ids(&self) -> Result<Vec<String>, String> {
        let list = installed_applications_list();
        Ok(list
            .iter()
            .map(|i| i.path.to_string_lossy().into_owned())
            .collect())
    }

    fn application_icon_png(&self, bundle_id: &str) -> Result<Vec<u8>, String> {
        let entry = look_up_application(bundle_id)
            .ok_or_else(|| "Unable to locate application.".to_string())?;

        entry
            .icon_png
            .ok_or_else(|| "Found application but it was missing an icon.".to_string())
    }

    fn launch_app_bundle(&self, bundle_id: &str) -> Result<(), String> {
        Command::new(bundle_id)
            .spawn()
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    /// Search for an application in the OS' global list of installed apps.
    fn search_apps(&self, query: &str) -> Result<Vec<AppSearchResult>, String> {
        let list = installed_applications_list();
        let query = query.trim();

        if query.is_empty() {
            return Ok(list.iter().map(|entry| entry.to_search_result()).collect());
        }

        if let Some(result) = list
            .iter()
            .find(|result| result.path.to_string_lossy() == query)
            .cloned()
        {
            return Ok(vec![result.to_search_result()]);
        }

        let lower_query = query.to_lowercase();
        Ok(list
            .iter()
            .filter(|result| {
                result.display_name.to_lowercase().contains(&lower_query)
                    || result
                        .path
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&lower_query)
            })
            .map(|entry| entry.to_search_result())
            .collect())
    }
}

/// Reports whether `hwnd` is in its GUI thread's modal move-or-resize loop.
///
/// Win32 combines moving and resizing under `GUI_INMOVESIZE`. Checking `hwndMoveSize` ensures a
/// different window owned by the same GUI thread does not produce a false positive.
fn window_is_moving(hwnd: HWND) -> WindowsResult<bool> {
    unsafe {
        let thread_id = GetWindowThreadProcessId(hwnd, None);
        if thread_id == 0 {
            return Err(windows::core::Error::from_thread());
        }

        let mut thread_info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(thread_id, &mut thread_info)?;

        Ok(thread_info.flags.contains(GUI_INMOVESIZE) && thread_info.hwndMoveSize == hwnd)
    }
}

fn get_focused_monitor_scale() -> f64 {
    unsafe {
        let window = GetForegroundWindow();
        let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);

        let mut x = 0;
        let mut y = 0;

        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y);

        x as f64 / 96.
    }
}

#[derive(Debug, Clone)]
struct ApplicationListEntry {
    path: PathBuf,
    /// The PNG bytes of an icon. 256 on a side, square.
    icon_png: Option<Vec<u8>>,
    display_name: String,
}

impl ApplicationListEntry {
    fn to_search_result(&self) -> AppSearchResult {
        AppSearchResult {
            name: self.display_name.clone(),
            bundle_id: self.path.to_string_lossy().into_owned(),
        }
    }
}

fn look_up_application(bundle_id: &str) -> Option<ApplicationListEntry> {
    // In Windows, the application path is the bundle ID.
    let list = installed_applications_list();
    list.iter()
        .find(|entry| entry.path.to_string_lossy() == bundle_id)
        .cloned()
}

#[cached]
fn installed_applications_list() -> Arc<Vec<ApplicationListEntry>> {
    let mut list = Vec::new();

    for app in gatherer().scan().flatten() {
        let icon = if let Ok(icon) = app.entry.icon() {
            icon.extract_icon_as_png_at(IconSize::Jumbo)
        } else {
            None
        };

        list.push(ApplicationListEntry {
            path: app.entry.path().to_owned(),
            icon_png: icon,
            display_name: app.entry.display_name(),
        })
    }

    Arc::new(list)
}

fn gatherer() -> Gatherer {
    Gatherer::new()
        .with_desktop(Priority(1.0))
        .with_start_menu(Priority(1.5))
        .with_windows_apps(Priority(2.0))
}

/// Returns the full executable path for the process that owns `hwnd`.
pub fn get_window_path(window_id: isize) -> WindowsResult<PathBuf> {
    unsafe {
        let mut process_id = 0;
        GetWindowThreadProcessId(HWND(window_id as *mut c_void), Some(&mut process_id));

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id)?;

        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        let path_result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        );
        let close_result = CloseHandle(process);

        path_result?;
        close_result?;

        Ok(PathBuf::from(OsString::from_wide(
            &buffer[..length as usize],
        )))
    }
}

impl WindowsBroker {
    /// Reads the focused app and turns what it finds into placed lints.
    ///
    /// Split out from `get_boxes` so the whole read is timed on every path out of it, including
    /// the ones that give up early.
    fn read_boxes(
        &mut self,
        lint_text: &mut dyn FnMut(&str) -> BTreeMap<String, Vec<Lint>>,
    ) -> Option<Vec<ActionableLint>> {
        match self.should_lint_focused_window() {
            Some(true) => {}
            Some(false) => return Some(Vec::new()),
            None => return None,
        }

        let text = self.service.lock().ok()?.get_text()?;
        if text.len() > 16_000 {
            return Some(Vec::new());
        }

        if !self.text_has_settled(&text) {
            // Returning nothing leaves the highlights from the previous read on screen, so they
            // stay put while the user is typing instead of flickering, and are brought back into
            // step as soon as there is a pause.
            return None;
        }

        let lints = match &self.cached_lints {
            Some((linted_text, lints)) if *linted_text == text => lints.clone(),
            _ => {
                let lints = lint_text(&text);
                self.cached_lints = Some((text.clone(), lints.clone()));

                lints
            }
        };
        let rects = self
            .service
            .lock()
            .ok()?
            .get_bounding_boxes(&text, lints.values().flatten().map(|lint| lint.span))?;

        Some(
            lints
                .into_iter()
                .flat_map(|(lint_id, lints)| {
                    lints.into_iter().map(move |lint| (lint_id.clone(), lint))
                })
                .zip(rects)
                .flat_map(|((lint_id, lint), rects)| {
                    let text = text.clone();
                    let service = self.service.clone();
                    rects.into_iter().map(move |rect| {
                        let service = service.clone();
                        let suggestion_text = text.clone();
                        let suggestion_span = lint.span;
                        ActionableLint::new(
                            rect,
                            lint_id.clone(),
                            lint.clone(),
                            text.clone(),
                            move |suggestion| {
                                service.lock().unwrap().apply_suggestion(
                                    suggestion_text,
                                    suggestion_span,
                                    suggestion,
                                );
                            },
                        )
                    })
                })
                .collect(),
        )
    }
}
