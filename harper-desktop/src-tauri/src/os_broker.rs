use harper_core::{
    Span,
    linting::{Lint, Suggestion},
};
use serde::Serialize;
use std::collections::BTreeMap;

use crate::rect::ActionableLint;

pub type LintText = Box<dyn FnMut(&str) -> BTreeMap<String, Vec<Lint>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AccessibilityPermissionStatus {
    Granted,
    NotGranted,
    Unsupported,
}

/// Provides platform-specific state needed by the highlighter without coupling rendering to an OS.
///
/// The highlighter needs both accessibility-derived lint rectangles and global cursor position, but
/// those APIs are platform-specific. This trait keeps the event loop and renderer independent from
/// macOS accessibility and pointer APIs.
pub trait OsBroker {
    /// Identifies Harper for automatic-discovery exclusion, not for manual integration policy.
    ///
    /// Uses the configured bundle ID on macOS and the current executable path on Windows.
    /// Unsupported platforms return `false`. No broker instance is needed; `Self: Sized` keeps
    /// the trait usable as `dyn OsBroker`.
    fn is_harper_desktop(_app_id: &str) -> bool
    where
        Self: Sized,
    {
        false
    }

    /// Get the actionable lint boxes from the OS, provided a linting source.
    ///
    /// `None` means the accessibility read failed and the last successful result should be retained.
    fn get_boxes(
        &mut self,
        lint_text: &mut dyn FnMut(&str) -> BTreeMap<String, Vec<Lint>>,
    ) -> Option<Vec<ActionableLint>>;

    /// How far the focused window has moved since this was last asked, in logical pixels.
    ///
    /// Underline positions are read from the focused app only a few times a second, so a window
    /// that is being dragged leaves them behind. Platforms that can report the move cheaply let
    /// the overlay carry the underlines along until the next real read catches up; the rest
    /// report nothing and keep the old behaviour.
    fn window_shift(&mut self) -> Option<(f64, f64)> {
        None
    }

    /// What the user has selected in the focused field, if anything.
    ///
    /// The review panel is opened from a selection, so a platform that cannot report one simply
    /// never offers the button, and nothing else about the overlay changes.
    fn selection(&mut self) -> Option<crate::rect::Selection> {
        None
    }

    /// Grab the position of the user's cursor on the screen.
    fn cursor_position(&self) -> Option<egui::Pos2>;

    /// Applies every one of `edits` to the focused text element in one go.
    ///
    /// `source_text` is the text the lints were produced from, so the platform can refuse the
    /// whole batch if the field changed underneath. Each edit is a span of that text and the
    /// suggestion to put in its place; spans must not overlap.
    fn apply_all_suggestions(&mut self, source_text: &str, edits: &[(Span<char>, Suggestion)]);

    /// Whether the user is pressing the primary pointer button right now.
    ///
    /// The overlay is click-through wherever Harper has nothing to offer, so clicks outside a
    /// highlight or popup never arrive as window events. Polling the button is what lets an open
    /// popup notice that the user clicked somewhere else and get out of the way.
    ///
    /// A platform that cannot report this returns `false`, which leaves the popup open until it is
    /// dismissed another way.
    fn pointer_pressed(&self) -> bool;

    /// Check whether Harper has permission to access the OS' native accessibility API.
    fn accessibility_permission_status(&self) -> AccessibilityPermissionStatus;

    /// Request permission to access the OS' native accessibility API.
    fn request_accessibility_permission(&self) -> AccessibilityPermissionStatus;

    /// Given an application identifier, find that integration's human-readable name.
    fn integration_display_name(&self, bundle_id: &str) -> String {
        bundle_id.to_owned()
    }

    /// Returns the bundle identifiers for installed graphical applications.
    ///
    /// Implementations should return stable bundle ID strings, sorted and deduplicated where
    /// possible. Platforms that do not support bundle IDs should return an error.
    fn installed_application_bundle_ids(&self) -> Result<Vec<String>, String>;

    /// Returns the application icon for `bundle_id` encoded as PNG bytes.
    ///
    /// The broker returns raw bytes so callers can choose their own transport format, such as a
    /// Tauri command converting them into a data URL.
    fn application_icon_png(&self, _bundle_id: &str) -> Result<Vec<u8>, String>;

    /// Start an application given its bundle ID.
    fn launch_app_bundle(&self, _bundle_id: &str) -> Result<(), String>;

    /// Search for an application in the OS' global list of installed apps.
    fn search_apps(&self, _query: &str) -> Result<Vec<AppSearchResult>, String>;
}

#[derive(Debug, Clone, Serialize)]
pub struct AppSearchResult {
    pub name: String,
    pub bundle_id: String,
}

/// No-op platform broker for targets that do not have an OS implementation yet.
///
/// This lets the highlighter compile on non-macOS platforms while making it explicit that there is
/// currently no accessibility or cursor integration there.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[derive(Default)]
pub struct NoopBroker;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl OsBroker for NoopBroker {
    fn get_boxes(
        &mut self,
        _lint_text: &mut dyn FnMut(&str) -> BTreeMap<String, Vec<Lint>>,
    ) -> Option<Vec<ActionableLint>> {
        Some(Vec::new())
    }

    fn cursor_position(&self) -> Option<egui::Pos2> {
        None
    }

    fn apply_all_suggestions(&mut self, _source_text: &str, _edits: &[(Span<char>, Suggestion)]) {}

    fn pointer_pressed(&self) -> bool {
        false
    }

    fn accessibility_permission_status(&self) -> AccessibilityPermissionStatus {
        AccessibilityPermissionStatus::Unsupported
    }

    fn request_accessibility_permission(&self) -> AccessibilityPermissionStatus {
        AccessibilityPermissionStatus::Unsupported
    }

    fn installed_application_bundle_ids(&self) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    fn application_icon_png(&self, _bundle_id: &str) -> Result<Vec<u8>, String> {
        Err("Cannot get application icons.".to_string())
    }

    fn launch_app_bundle(&self, _bundle_id: &str) -> Result<(), String> {
        Ok(())
    }

    fn search_apps(&self, _query: &str) -> Result<Vec<AppSearchResult>, String> {
        Ok(Vec::new())
    }
}
