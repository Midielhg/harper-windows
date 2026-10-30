use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
#[cfg(target_os = "macos")]
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
use winit::window::WindowId;

use super::AddToDictionary;
use super::DisableRule;
use super::Error;
use super::IgnoreLint;
use super::OpenSettings;
use super::RefreshConfig;
use super::fix_all_hotkey::FixAllHotkey;
use super::render_state::{ApplyAllRequest, HitTarget, RenderState};
use super::window::Window;
use crate::os_broker::{LintText, OsBroker};
use crate::rect::ActionableLint;

const CONFIG_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How long the event loop sleeps between passes when nothing has woken it.
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// Owns the winit event loop and the overlay windows created for each monitor.
///
/// `WindowManager` is intentionally separate from `Highlighter` because winit event-loop ownership
/// is a process-level concern. It also keeps monitor enumeration, native window lifecycle, and event
/// dispatch out of the public highlighter API.
pub struct WindowManager {
    event_loop: EventLoop<()>,
    rects: Vec<ActionableLint>,
    os_broker: Box<dyn OsBroker>,
    lint_text: LintText,
    ignore_lint: IgnoreLint,
    add_to_dictionary: AddToDictionary,
    disable_rule: DisableRule,
    refresh_config: RefreshConfig,
    open_settings: OpenSettings,
}

pub struct WindowManagerCallbacks {
    pub lint_text: LintText,
    pub ignore_lint: IgnoreLint,
    pub add_to_dictionary: AddToDictionary,
    pub disable_rule: DisableRule,
    pub refresh_config: RefreshConfig,
    pub open_settings: OpenSettings,
}

impl WindowManager {
    /// Creates the event loop before windows exist because winit requires window creation to happen
    /// from inside that loop's lifecycle callbacks.
    pub fn new(
        os_broker: Box<dyn OsBroker>,
        callbacks: WindowManagerCallbacks,
    ) -> Result<Self, Error> {
        let mut event_loop_builder = EventLoop::builder();

        #[cfg(target_os = "macos")]
        event_loop_builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_default_menu(false);

        Ok(Self {
            event_loop: event_loop_builder.build()?,
            rects: Vec::new(),
            os_broker,
            lint_text: callbacks.lint_text,
            ignore_lint: callbacks.ignore_lint,
            add_to_dictionary: callbacks.add_to_dictionary,
            disable_rule: callbacks.disable_rule,
            refresh_config: callbacks.refresh_config,
            open_settings: callbacks.open_settings,
        })
    }

    /// Seeds externally supplied lint rectangles before the event loop takes ownership of rendering.
    pub fn set_rects(&mut self, rects: Vec<ActionableLint>) {
        self.rects = rects;
    }

    /// Transfers ownership into winit's application handler because `run_app` owns the process-level
    /// event loop until the overlay exits.
    pub fn run_window_for_each_monitor(self) -> Result<(), Error> {
        let mut app = WindowManagerApp::new(
            self.rects,
            self.os_broker,
            WindowManagerCallbacks {
                lint_text: self.lint_text,
                ignore_lint: self.ignore_lint,
                add_to_dictionary: self.add_to_dictionary,
                disable_rule: self.disable_rule,
                refresh_config: self.refresh_config,
                open_settings: self.open_settings,
            },
        );

        self.event_loop.set_control_flow(ControlFlow::Poll);
        let result = self.event_loop.run_app(&mut app);

        if let Some(error) = app.error {
            return Err(error);
        }

        result.map_err(Error::from)
    }
}

struct WindowManagerApp {
    windows: Vec<Window>,
    render_state: RenderState,
    os_broker: Box<dyn OsBroker>,
    lint_text: LintText,
    last_config_poll: Instant,
    refresh_config: RefreshConfig,
    hovered_lint: Option<usize>,
    cursor_hittest_enabled: bool,
    /// Whether the primary pointer button was already down at the previous poll, so one press is
    /// acted on once instead of for every frame the user holds the button.
    pointer_was_pressed: bool,
    /// Watches for the shortcut that applies every suggestion at once.
    fix_all_hotkey: FixAllHotkey,
    error: Option<Error>,
}

impl WindowManagerApp {
    /// Builds the mutable application state consumed by winit callbacks after `WindowManager` gives
    /// up direct control of the event loop.
    fn new(
        rects: Vec<ActionableLint>,
        os_broker: Box<dyn OsBroker>,
        callbacks: WindowManagerCallbacks,
    ) -> Self {
        Self {
            windows: Vec::new(),
            render_state: RenderState::new(
                rects,
                callbacks.ignore_lint,
                callbacks.add_to_dictionary,
                callbacks.disable_rule,
                callbacks.open_settings,
            ),
            os_broker,
            lint_text: callbacks.lint_text,
            last_config_poll: Instant::now(),
            refresh_config: callbacks.refresh_config,
            hovered_lint: None,
            pointer_was_pressed: false,
            fix_all_hotkey: FixAllHotkey::install(),
            cursor_hittest_enabled: false,
            error: None,
        }
    }

    /// Refreshes lint geometry from the OS broker inside the event loop so repaint requests happen on
    /// the same thread that owns the overlay windows.
    fn read_rect_updates(&mut self) {
        // Done before the read, not after it, so a read that comes back in the same frame has the
        // last word: it knows where the text actually is, and this only guesses.
        if let Some((dx, dy)) = self.os_broker.window_shift() {
            self.render_state.translate_lints(dx, dy);
        }

        let lints = self.os_broker.get_boxes(self.lint_text.as_mut());
        if let Some(lints) = lints {
            self.render_state.set_lints(lints);
        }

        let selection = self.os_broker.selection();
        self.render_state.set_selection(selection);

        for window in &self.windows {
            window.request_redraw();
        }
    }

    fn refresh_config(&mut self) {
        (self.refresh_config)();
    }

    /// Toggles native click-through behavior based on the cursor's current interactive Harper target,
    /// keeping editor clicks from being swallowed when the pointer is outside highlights and popups.
    fn update_cursor_hittest(&mut self, event_loop: &ActiveEventLoop) {
        let Some(cursor_pos) = self.os_broker.cursor_position() else {
            return;
        };

        let hit_target = self.render_state.hit_target_at_pos(cursor_pos);

        self.hovered_lint = match hit_target {
            HitTarget::Lint(index) => Some(index),
            // Keep the chip's own lint hovered while the pointer is on the chip, so moving to it
            // does not make it disappear.
            HitTarget::QuickFix(index) => Some(index),
            HitTarget::Popup
            | HitTarget::ReviewPanel
            | HitTarget::ReviewButton
            | HitTarget::None => None,
        };

        self.render_state.set_hovered_lint(self.hovered_lint);

        self.close_popup_on_outside_click(hit_target);

        let should_enable_hittest = !matches!(hit_target, HitTarget::None);

        if self.cursor_hittest_enabled == should_enable_hittest {
            return;
        }

        for window in &self.windows {
            if let Err(error) = window.set_cursor_hittest(should_enable_hittest) {
                self.error = Some(error);
                event_loop.exit();
                return;
            }
        }

        self.cursor_hittest_enabled = should_enable_hittest;
    }

    /// Applies every suggestion in the focused field when the shortcut asks for it.
    ///
    /// The shortcut is only live while a popup is open, so the key keeps its usual meaning the
    /// rest of the time. Lints that only point something out, such as a sentence being too long,
    /// carry no suggestion and are left alone.
    fn apply_all_if_requested(&mut self) {
        self.fix_all_hotkey
            .set_armed(self.render_state.offers_apply_all());

        // Taken whether or not it is used, so a press that arrives in the same frame as a click
        // on the panel is spent here rather than left to fire a whole-field apply next frame.
        let pressed_shortcut = self.fix_all_hotkey.take_request();

        // The panel's own request already names what it listed. The shortcut names nothing itself,
        // so it takes its meaning from what is on screen: the review offer when there is one, and
        // everything in the field when there is not.
        let request = match self.render_state.take_apply_all_request() {
            Some(request) => Some(request),
            None if pressed_shortcut => match self.render_state.take_review_apply_all_request() {
                Some(request) => Some(request),
                None => self.whole_field_request(),
            },
            None => return,
        };

        let Some(request) = request else {
            return;
        };

        let ApplyAllRequest { source_text, edits } = request;

        self.os_broker.apply_all_suggestions(&source_text, &edits);
        self.render_state.close_popup();

        for window in &self.windows {
            window.request_redraw();
        }
    }

    /// Every fix available in the focused field, for the shortcut that asks for all of them.
    ///
    /// Lints that only point something out, such as a sentence being too long, carry no suggestion
    /// and are left alone.
    fn whole_field_request(&self) -> Option<ApplyAllRequest> {
        let lints = self.render_state.all_lints();
        let source_text = lints.first()?.source_text.clone();

        let edits: Vec<_> = lints
            .iter()
            .filter_map(|actionable_lint| {
                let suggestion = actionable_lint.lint.suggestions.first()?;

                Some((actionable_lint.lint.span, suggestion.clone()))
            })
            .collect();

        (!edits.is_empty()).then_some(ApplyAllRequest { source_text, edits })
    }

    /// Dismisses an open popup when the user clicks anywhere that is not Harper's own.
    ///
    /// Those clicks never reach the overlay, which is click-through outside its highlights and
    /// popup, so the press is noticed by polling rather than by a window event.
    fn close_popup_on_outside_click(&mut self, hit_target: HitTarget) {
        let pressed = self.os_broker.pointer_pressed();
        let just_pressed = pressed && !self.pointer_was_pressed;
        self.pointer_was_pressed = pressed;

        if !just_pressed || !matches!(hit_target, HitTarget::None) {
            return;
        }

        self.render_state.close_popup();

        for window in &self.windows {
            window.request_redraw();
        }
    }

    /// Opens or replaces the popup only when the actual click target is a lint, leaving popup clicks
    /// for egui controls such as the close button.
    fn select_hit_lint(&mut self) {
        let Some(cursor_pos) = self.os_broker.cursor_position() else {
            return;
        };

        let HitTarget::Lint(index) = self.render_state.hit_target_at_pos(cursor_pos) else {
            return;
        };

        self.render_state.set_highlighted_lint(Some(index));

        for window in &self.windows {
            window.request_redraw();
        }
    }
}

impl ApplicationHandler for WindowManagerApp {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();

        self.read_rect_updates();

        if now.duration_since(self.last_config_poll) >= CONFIG_POLL_INTERVAL {
            self.refresh_config();
            self.last_config_poll = now;
        }

        self.update_cursor_hittest(event_loop);
        self.apply_all_if_requested();

        // Sleep until the next frame instead of spinning. The reads this loop drives are spaced
        // out by the broker, so without a wait the loop just burned a core asking for work that
        // was not due yet.
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + FRAME_INTERVAL));
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.windows.is_empty() {
            return;
        }

        let monitors = event_loop.available_monitors().collect::<Vec<_>>();

        for monitor in monitors {
            match pollster::block_on(Window::new(event_loop, monitor)) {
                Ok(window) => self.windows.push(window),
                Err(error) => {
                    self.error = Some(error);
                    event_loop.exit();
                    return;
                }
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let should_select_hit_lint = matches!(
            &event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        );
        let should_render = matches!(&event, WindowEvent::RedrawRequested);

        if let Some(window) = self
            .windows
            .iter_mut()
            .find(|window| window.id() == window_id)
        {
            window.handle_event(&event);

            if should_render {
                window.render(&mut self.render_state);
            }
        }

        if should_select_hit_lint {
            self.select_hit_lint();
        }

        if matches!(&event, WindowEvent::CloseRequested) {
            event_loop.exit();
        }
    }
}
