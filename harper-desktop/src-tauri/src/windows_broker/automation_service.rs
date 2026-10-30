use std::cell::RefCell;
use std::iter::once;
use std::sync::mpsc::{
    Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError, sync_channel,
};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::rect::{Rect, Selection};
use crate::timing;
use crate::windows_broker::clipboard::ClipboardText;
use crate::windows_broker::get_focused_monitor_scale;
use harper_core::{Span, linting::Suggestion};
use is_macro::Is;
use uiautomation::types::{Handle, TextPatternRangeEndpoint, TextUnit, TreeScope, UIProperty};
use uiautomation::variants::Variant;
use uiautomation::{
    UIAutomation, UIElement,
    patterns::{UITextPattern, UITextRange, UIValuePattern},
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::IUIAutomationTextRange;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// How long the drawing thread waits for a job before leaving it to finish on its own.
///
/// Comfortably longer than a read, which runs in a few milliseconds, and far shorter than the
/// stall an app that has stopped answering would otherwise impose on the overlay.
const WORKER_TIMEOUT: Duration = Duration::from_millis(250);

/// Information about a worker thread.
struct WorkerData {
    sender: SyncSender<(WorkerJob, Vec<JobArgument>)>,
    receiver: Receiver<JobResult>,
}

#[derive(Debug)]
struct ApplySuggestionRequest {
    window: isize,
    expected_text: String,
    span: Span<char>,
    suggestion: Suggestion,
}

#[derive(Debug)]
struct ApplyAllRequest {
    window: isize,
    expected_text: String,
    edits: Vec<(Span<char>, Suggestion)>,
}

#[derive(Debug, Is)]
enum JobArgument {
    Span(Span<char>),
    Window(isize),
    Text(String),
    ApplySuggestion(ApplySuggestionRequest),
    ApplyAll(ApplyAllRequest),
}

/// The result of a job run by the worker thread.
#[derive(Debug, Is)]
enum JobResult {
    None,
    String(String),
    GroupedRects(Vec<Vec<Rect>>),
    Selection(Option<Selection>),
    Err,
}

/// An actual function pointer to be run by the worker thread.
type WorkerJob = fn(&UIAutomation, Vec<JobArgument>) -> JobResult;

/// Runs and communicates with a worker thread to interact with the Win32 Automation API to query the accessibility tree.
/// Necessary because the API has very specific thread setting requirements to work.
pub struct AutomationService {
    worker_data: Option<WorkerData>,
    /// Results of jobs that were sent without waiting for them, still to be taken off the channel.
    pending_results: usize,
    // Needed to redirect focus to the last focused window when the focus arrives on the Harper highlighter window
    last_focused_window: Option<isize>,
}

impl AutomationService {
    pub fn create_and_start() -> Self {
        let mut output = Self {
            last_focused_window: None,
            worker_data: None,
            pending_results: 0,
        };

        output.start_worker_thread();

        output
    }

    /// Starts the worker thread if it is not already running.
    /// Does nothing if the worker thread is already running.
    fn start_worker_thread(&mut self) {
        let (job_sender, job_receiver) = sync_channel::<(WorkerJob, Vec<JobArgument>)>(1);
        let (result_sender, result_receiver) = sync_channel(1);

        std::thread::spawn(move || {
            let automation = UIAutomation::new().unwrap();

            loop {
                // Stop the thread if the other side of the channel has been closed (or dropped).
                let job = match job_receiver.try_recv() {
                    Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => None,
                    Ok(job) => Some(job),
                };

                if let Some((job, arguments)) = job {
                    let result = job(&automation, arguments);

                    // Stop the thread if the other side of the channel has been closed (or dropped).
                    if let Err(TrySendError::Disconnected(_)) = result_sender.try_send(result) {
                        break;
                    }
                }

                sleep(Duration::from_millis(16));
            }
        });

        self.worker_data = Some(WorkerData {
            receiver: result_receiver,
            sender: job_sender,
        });
    }

    /// Stops the worker thread if it is running. This method does nothing if it is not running.
    fn stop_worker_thread(&mut self) {
        // This drops the inner fields, which closes the channel, which signals to the worker to stop running.
        self.worker_data = None;
    }

    /// Attempts to run a worker job on the worker thread.
    ///
    /// Returns `None` when the worker is not there, is still busy, or did not answer in time. The
    /// caller is the thread drawing the overlay, and it has to keep drawing: a read it misses is
    /// one frame of slightly stale highlights, while a read it waits on is a frozen overlay.
    fn run_worker_job(&mut self, job: WorkerJob, arguments: Vec<JobArgument>) -> Option<JobResult> {
        // A result still owed from an earlier job would be handed back as this one's, so the job
        // is not sent at all until the channel is clear.
        if !self.take_pending_results() {
            return None;
        }

        let worker_data = self.worker_data.as_ref()?;

        match worker_data.sender.try_send((job, arguments)) {
            Ok(()) => {}
            // The worker has not picked up its last job yet. Nothing is owed, so there is simply
            // nothing to do this frame.
            Err(TrySendError::Full(_)) => return None,
            Err(TrySendError::Disconnected(_)) => {
                self.stop_worker_thread();
                return None;
            }
        }

        match worker_data.receiver.recv_timeout(WORKER_TIMEOUT) {
            Ok(result) => Some(result),
            // The job is still running and will answer eventually, so the result is counted as
            // owed and collected later rather than waited for now. An accessibility call into an
            // app that has stopped answering can take as long as it likes, and before this it took
            // the overlay down with it until the whole app was restarted.
            Err(RecvTimeoutError::Timeout) => {
                self.pending_results += 1;
                None
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.stop_worker_thread();
                None
            }
        }
    }

    /// Hands a job to the worker without waiting for it to finish.
    ///
    /// Editing text takes a while on purpose: the worker holds on until the app has taken every
    /// keystroke, so the lint loop cannot interleave with them. That wait belongs on the worker,
    /// not on the thread drawing the overlay, which would otherwise freeze until the edit is done
    /// and make a fix that takes milliseconds feel like it takes half a second.
    fn dispatch_worker_job(&mut self, job: WorkerJob, arguments: Vec<JobArgument>) {
        self.take_pending_results();

        let Some(worker_data) = self.worker_data.as_ref() else {
            return;
        };

        if worker_data.sender.try_send((job, arguments)).is_ok() {
            self.pending_results += 1;
        }
    }

    /// Clears the results of jobs nobody waited for, so a later job gets its own result back.
    ///
    /// Returns whether everything owed has now been collected. Nothing here blocks: an edit can
    /// hold the worker for a second or more, and this runs on the thread drawing the overlay, so
    /// waiting for one would stop the overlay for exactly as long as the edit took.
    fn take_pending_results(&mut self) -> bool {
        while self.pending_results > 0 {
            let Some(worker_data) = self.worker_data.as_ref() else {
                self.pending_results = 0;
                return true;
            };

            match worker_data.receiver.try_recv() {
                Ok(_) => self.pending_results -= 1,
                // Still working. It will be collected on a later frame.
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => {
                    self.pending_results = 0;
                    return true;
                }
            }
        }

        true
    }

    /// Grab text from the worker.
    /// Attempts to get the most up-to-date information possible.
    /// Returns `None` if the worker is not running.
    pub fn get_text(&mut self) -> Option<String> {
        let window = self.resolve_focused_window()?;
        let result = self.run_worker_job(get_text_job, vec![JobArgument::Window(window)])?;

        match result {
            JobResult::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn apply_suggestion(
        &mut self,
        expected_text: String,
        span: Span<char>,
        suggestion: Suggestion,
    ) {
        let Some(window) = self.resolve_focused_window() else {
            return;
        };

        let request = ApplySuggestionRequest {
            window,
            expected_text,
            span,
            suggestion,
        };

        self.dispatch_worker_job(
            apply_suggestion_job,
            vec![JobArgument::ApplySuggestion(request)],
        );
    }

    /// Applies every edit in one pass over the focused element.
    ///
    /// The edits are applied from the end of the text backwards, so the positions of the ones
    /// still to come are not moved by the ones already made.
    pub fn apply_suggestions(
        &mut self,
        expected_text: String,
        edits: Vec<(Span<char>, Suggestion)>,
    ) {
        let Some(window) = self.resolve_focused_window() else {
            return;
        };

        let request = ApplyAllRequest {
            window,
            expected_text,
            edits,
        };

        self.dispatch_worker_job(apply_all_job, vec![JobArgument::ApplyAll(request)]);
    }

    /// Pass a collection of text spans to the worker and have it compute the associated bounding boxes for each span.
    /// Each span may have multiple bounding boxes.
    /// Input spans share the same index as their output bounding box.
    pub fn get_bounding_boxes(
        &mut self,
        text: &str,
        spans: impl IntoIterator<Item = Span<char>>,
    ) -> Option<Vec<Vec<Rect>>> {
        let window = self.resolve_focused_window()?;

        let result = self.run_worker_job(
            get_bounding_rect_job,
            once(JobArgument::Window(window))
                .chain(once(JobArgument::Text(text.to_string())))
                .chain(spans.into_iter().map(JobArgument::Span))
                .collect(),
        )?;

        match result {
            JobResult::GroupedRects(rects) => Some(rects),
            _ => None,
        }
    }

    /// What the user has selected in the focused field, if anything.
    pub fn get_selection(&mut self) -> Option<Selection> {
        let window = self.resolve_focused_window()?;
        let result = self.run_worker_job(get_selection_job, vec![JobArgument::Window(window)])?;

        match result {
            JobResult::Selection(selection) => selection,
            _ => None,
        }
    }

    /// Returns the foreground source window, retaining the last external window while Harper's
    /// overlay owns focus.
    pub fn resolve_focused_window(&mut self) -> Option<isize> {
        let (focused_window, focused_process_id) = focused_window()?;

        if focused_process_id == std::process::id() {
            return self.last_focused_window;
        }

        self.last_focused_window = Some(focused_window);
        Some(focused_window)
    }
}

impl Drop for AutomationService {
    fn drop(&mut self) {
        self.stop_worker_thread();
    }
}

fn apply_suggestion_job(automation: &UIAutomation, mut arguments: Vec<JobArgument>) -> JobResult {
    let Some(JobArgument::ApplySuggestion(request)) = arguments.pop() else {
        return JobResult::Err;
    };

    if !arguments.is_empty() {
        return JobResult::Err;
    }

    let _job = timing::step("apply-one", None);

    let element = {
        let _step = timing::step("find-element", None);

        let Ok(element) =
            text_element_for_window(automation, request.window, Some(&request.expected_text))
        else {
            eprintln!(
                "Unable to apply Windows suggestion: the source text element is no longer available"
            );
            return JobResult::None;
        };

        element
    };

    let current_text = {
        let _step = timing::step("read-text", None);

        let Ok(current_text) = get_text(&element) else {
            eprintln!("Unable to apply Windows suggestion: the source text can no longer be read");
            return JobResult::None;
        };

        current_text
    };

    let updated_text = match apply_suggestion_to_text(
        &current_text,
        &request.expected_text,
        request.span,
        &request.suggestion,
    ) {
        Ok(updated_text) => updated_text,
        Err(error) => {
            eprintln!("Unable to apply Windows suggestion: {error}");
            return JobResult::None;
        }
    };

    // Replace the lint's own characters rather than rewriting the whole value. Notepad reports a
    // writable value pattern and then ignores what is written to it, and most editors built on a
    // web view expose no value pattern at all, so setting the value is not something to rely on.
    // Selecting the span and typing over it is what any control that accepts keyboard input
    // supports, and it leaves the app's own undo history intact.
    let replacement = replacement_for_span(&current_text, request.span, &request.suggestion);

    {
        let _step = timing::step("focus", None);

        if let Err(error) = element.set_focus() {
            eprintln!("Unable to focus the field to apply the Windows suggestion: {error}");
            return JobResult::None;
        }
    }

    if let Err(error) = replace_span_by_typing(&element, &current_text, request.span, &replacement)
    {
        eprintln!("Unable to type the Windows suggestion: {error}");

        if let Ok(value_pattern) = element.get_pattern::<UIValuePattern>()
            && value_pattern.is_readonly() == Ok(false)
            && let Err(error) = value_pattern.set_value(&updated_text)
        {
            eprintln!("Unable to apply Windows suggestion: {error}");
        }
    }

    JobResult::None
}

/// Applies a whole batch of suggestions to the focused element.
///
/// Edits run from the end of the text backwards so that each span still refers to the same
/// characters when its turn comes, and overlapping edits are dropped rather than applied on top of
/// one another.
fn apply_all_job(automation: &UIAutomation, mut arguments: Vec<JobArgument>) -> JobResult {
    let Some(JobArgument::ApplyAll(request)) = arguments.pop() else {
        return JobResult::Err;
    };

    if !arguments.is_empty() {
        return JobResult::Err;
    }

    let _job = timing::step("apply-all", Some(format!("{} edits", request.edits.len())));

    let element = {
        let _step = timing::step("find-element", None);

        let Ok(element) =
            text_element_for_window(automation, request.window, Some(&request.expected_text))
        else {
            eprintln!("Unable to apply Windows suggestions: the source text changed");
            return JobResult::None;
        };

        element
    };

    // Read the field once for the whole batch. Every span was worked out against this text, so if
    // it has moved on since, the spans point at characters that are no longer there and applying
    // them would write over the wrong words.
    let _read = timing::step("read-text", None);
    match get_text(&element) {
        Ok(text) if text == request.expected_text => {}
        Ok(_) => {
            eprintln!("Unable to apply Windows suggestions: the source text changed");
            return JobResult::None;
        }
        Err(error) => {
            eprintln!("Unable to apply Windows suggestions: {error}");
            return JobResult::None;
        }
    }

    drop(_read);

    {
        let _step = timing::step("focus", None);

        if let Err(error) = element.set_focus() {
            eprintln!("Unable to focus the field to apply the Windows suggestions: {error}");
            return JobResult::None;
        }
    }

    let mut edits = request.edits;
    edits.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));

    let mut next_start = request.expected_text.chars().count();
    // What the field holds as the batch goes on. Each edit hands the next one its own answer, so
    // nobody has to ask the app.
    let mut current = request.expected_text.clone();

    for (span, suggestion) in edits {
        if span.end > next_start {
            continue;
        }

        // Edits run backwards, so everything before this span is still as it was when the
        // suggestion was worked out, and the original text describes it correctly.
        let replacement = replacement_for_span(&request.expected_text, span, &suggestion);

        let _edit = timing::step("edit", None);

        match replace_span_by_typing(&element, &current, span, &replacement) {
            Ok(text) => current = text,
            Err(error) => {
                eprintln!("Unable to apply one of the Windows suggestions: {error}");
                continue;
            }
        }

        next_start = span.start;
    }

    JobResult::None
}

/// How long to wait for typed input to show up in the element's own text.
const EDIT_TIMEOUT: Duration = Duration::from_millis(600);
/// How often to check while waiting.
const EDIT_POLL: Duration = Duration::from_millis(15);

/// Blocks until the element reads back `expected`, or the wait times out.
fn wait_for_text(element: &UIElement, expected: &str) {
    let deadline = Instant::now() + EDIT_TIMEOUT;

    while Instant::now() < deadline {
        if get_text(element).is_ok_and(|text| text == expected) {
            return;
        }

        sleep(EDIT_POLL);
    }
}

/// The text that should end up where the lint's span is now.
///
/// Selecting the span and typing this is equivalent to applying the suggestion, including for
/// `InsertAfter`, which keeps the original text and adds to it.
fn replacement_for_span(current_text: &str, span: Span<char>, suggestion: &Suggestion) -> String {
    match suggestion {
        Suggestion::ReplaceWith(chars) => chars.iter().collect(),
        Suggestion::InsertAfter(chars) => current_text
            .chars()
            .skip(span.start)
            .take(span.len())
            .chain(chars.iter().copied())
            .collect(),
        Suggestion::Remove => String::new(),
    }
}

/// Selects the characters in `span` and types `replacement` over them.
///
/// `before` is what the field holds right now, and the text it will hold once the keys land is
/// returned, so a batch can carry it from one edit to the next. Asking the app instead would cost
/// a cross-process call per edit, answered on the very thread that has the keystrokes to take.
///
/// The element must already hold keyboard focus, because the replacement is sent as ordinary key
/// input. An empty replacement deletes the selection instead of typing nothing.
fn replace_span_by_typing(
    element: &UIElement,
    before: &str,
    span: Span<char>,
    replacement: &str,
) -> Result<String> {
    if span.end > before.chars().count() {
        return Err(Error::new(
            uiautomation::errors::ERR_INVALID_ARG,
            "the lint span is outside the source text",
        ));
    }

    let range = {
        let _step = timing::step("range", None);
        matching_text_range(element, before, span)?
    };

    {
        let _step = timing::step("select", None);
        select_and_confirm(element, &range)?;
    }

    let expected = replaced_text(before, span, replacement);

    // Held until the replacement has landed, so the clipboard still holds it while the app reads
    // it, and goes back to whatever was on it once it has.
    let _clipboard;

    {
        let _step = timing::step(
            "send",
            Some(format!("{} chars", replacement.chars().count())),
        );

        if replacement.is_empty() {
            send_key(VK_BACK);
        } else {
            match paste_text(replacement) {
                Some(restore) => _clipboard = restore,
                None => send_text(replacement),
            }
        }
    }

    // Hold the worker until the app has taken every keystroke. An app answers accessibility
    // requests on the same thread it handles input on, so the lint loop — which runs on this
    // worker — reading its text while keys are still queued makes it alternate between the two,
    // and the replacement appears one slow character at a time.
    {
        let _step = timing::step(
            "wait",
            Some(format!("{} chars typed", replacement.chars().count())),
        );
        wait_for_text(element, &expected);
    }

    Ok(expected)
}

/// `text` with the characters in `span` replaced by `replacement`.
fn replaced_text(text: &str, span: Span<char>, replacement: &str) -> String {
    text.chars()
        .take(span.start)
        .chain(replacement.chars())
        .chain(text.chars().skip(span.end))
        .collect()
}

/// Number of times to set the selection before giving up on it holding.
const SELECTION_ATTEMPTS: usize = 4;
/// How long to let the app work through queued input between attempts.
const SELECTION_SETTLE: Duration = Duration::from_millis(40);

/// Selects `range` and returns once the app agrees that is what is selected.
///
/// The click that asked for the suggestion can still be on its way to the app, and when it lands
/// it moves the caret to wherever the pointer is. Typing then puts the replacement there instead
/// of over the mistake. Waiting for the selection to hold means the text goes where the highlight
/// was, no matter where the pointer happens to be.
fn select_and_confirm(element: &UIElement, range: &UITextRange) -> Result<()> {
    let mut last_error = None;

    for attempt in 0..SELECTION_ATTEMPTS {
        // Let any input the app has not processed yet arrive before trying again, so it cannot
        // undo the selection straight after it is made. The first attempt does not wait: most of
        // the time nothing is in flight and the delay would just be felt as lag.
        if attempt > 0 {
            sleep(SELECTION_SETTLE);
        }

        if let Err(error) = range.select() {
            last_error = Some(error);
            continue;
        }

        match selection_matches(element, range) {
            Ok(true) => return Ok(()),
            Ok(false) => {
                last_error = Some(Error::new(
                    uiautomation::errors::ERR_NOTFOUND,
                    "the app moved the selection away from the lint",
                ));
            }
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        Error::new(
            uiautomation::errors::ERR_NOTFOUND,
            "could not select the text to replace",
        )
    }))
}

/// Whether the app's current selection is exactly `range`.
fn selection_matches(element: &UIElement, range: &UITextRange) -> Result<bool> {
    let pattern: UITextPattern = element.get_pattern()?;

    for selected in pattern.get_selection()? {
        let same_start = selected.compare_endpoints(
            TextPatternRangeEndpoint::Start,
            range,
            TextPatternRangeEndpoint::Start,
        )?;
        let same_end = selected.compare_endpoints(
            TextPatternRangeEndpoint::End,
            range,
            TextPatternRangeEndpoint::End,
        )?;

        if same_start == 0 && same_end == 0 {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Below this many characters, typing is sent rather than pasted.
///
/// A paste costs one keystroke plus the work of borrowing the clipboard and giving it back, so
/// for a very short replacement typing is no slower and leaves the clipboard untouched.
const PASTE_FROM: usize = 3;

/// Puts `text` on the clipboard and pastes it, as one keystroke rather than one per character.
///
/// Returns the clipboard's restorer, which the caller holds until the paste has landed, or `None`
/// when the clipboard could not be borrowed — the caller then types the replacement instead, so a
/// busy clipboard costs speed rather than the fix.
fn paste_text(text: &str) -> Option<ClipboardText> {
    if text.chars().count() < PASTE_FROM {
        return None;
    }

    let restore = ClipboardText::set(text)?;

    send_inputs(&[
        key_input(VK_CONTROL, false),
        key_input(VK_V, false),
        key_input(VK_V, true),
        key_input(VK_CONTROL, true),
    ]);

    Some(restore)
}

/// Sends `text` as Unicode key input to whatever currently has keyboard focus.
fn send_text(text: &str) {
    let inputs: Vec<INPUT> = text
        .encode_utf16()
        .flat_map(|unit| [unicode_input(unit, false), unicode_input(unit, true)])
        .collect();

    send_inputs(&inputs);
}

fn send_key(key: VIRTUAL_KEY) {
    send_inputs(&[key_input(key, false), key_input(key, true)]);
}

fn send_inputs(inputs: &[INPUT]) {
    if inputs.is_empty() {
        return;
    }

    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };

    if sent as usize != inputs.len() {
        eprintln!(
            "Only {sent} of {} Windows input events were accepted",
            inputs.len()
        );
    }
}

fn unicode_input(unit: u16, key_up: bool) -> INPUT {
    keyboard_input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE, key_up)
}

fn key_input(key: VIRTUAL_KEY, key_up: bool) -> INPUT {
    keyboard_input(key, 0, KEYBD_EVENT_FLAGS(0), key_up)
}

fn keyboard_input(key: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: scan,
                dwFlags: if key_up {
                    flags | KEYEVENTF_KEYUP
                } else {
                    flags
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn apply_suggestion_to_text(
    current_text: &str,
    expected_text: &str,
    span: Span<char>,
    suggestion: &Suggestion,
) -> std::result::Result<String, &'static str> {
    if current_text != expected_text {
        return Err("the source text changed after linting");
    }

    let mut chars = current_text.chars().collect::<Vec<_>>();
    if span.end > chars.len() {
        return Err("the lint span is outside the source text");
    }

    suggestion.apply(span, &mut chars);
    Ok(chars.into_iter().collect())
}

fn get_text(element: &UIElement) -> uiautomation::Result<String> {
    let pattern: UITextPattern = element.get_pattern()?;
    let range = pattern.get_document_range()?;
    range.get_text(-1)
}

/// Finds the focused text element below `window`.
///
/// When `expected_text` is provided, unrelated text providers are excluded.
fn text_element_for_window(
    automation: &UIAutomation,
    window: isize,
    expected_text: Option<&str>,
) -> uiautomation::Result<UIElement> {
    if let Some(expected) = expected_text
        && let Some(element) = remembered_text_element(expected)
    {
        return Ok(element);
    }

    let root = automation.element_from_handle(Handle::from(window))?;
    let text_condition = automation.create_property_condition(
        UIProperty::IsTextPatternAvailable,
        Variant::from(true),
        None,
    )?;
    // Only look for the focused provider when there is nothing else to go on. Clicking a
    // suggestion moves keyboard focus to Harper's own popup, so by the time an edit is applied the
    // field it belongs to no longer reports focus; the text it held is what identifies it instead.
    let condition = match expected_text {
        Some(_) => text_condition,
        None => {
            let keyboard_condition = automation.create_property_condition(
                UIProperty::HasKeyboardFocus,
                Variant::from(true),
                None,
            )?;

            automation.create_and_condition(text_condition, keyboard_condition)?
        }
    };

    // An app can offer several focused text providers at once. A browser, and any app built on a
    // web view, reports its whole page as a read-only document alongside the field the caret is
    // in, and the document comes first. Taking that one lints every word on screen and offers
    // suggestions that cannot be applied, so prefer a provider the user can actually type into.
    let mut fallback = None;

    for element in root.find_all(TreeScope::Subtree, &condition)? {
        if let Some(expected) = expected_text {
            let Ok(text) = get_text(&element) else {
                continue;
            };

            if expected != text {
                continue;
            }
        }

        match is_read_only(&element) {
            Some(false) => return Ok(element),
            // Keep providers that do not answer the question as a last resort, so an editor with
            // an unusual accessibility implementation keeps working as it did before.
            None => fallback = fallback.or(Some(element)),
            Some(true) => {}
        }
    }

    fallback.ok_or_else(|| {
        Error::new(
            uiautomation::errors::ERR_NOTFOUND,
            "no editable text element found",
        )
    })
}

/// Whether `element` refuses edits, as far as it is willing to say.
///
/// `None` means the element does not report the property at all, which is not the same as being
/// writable and is treated separately by the caller.
fn is_read_only(element: &UIElement) -> Option<bool> {
    element
        .get_property_value(UIProperty::ValueIsReadOnly)
        .ok()
        .and_then(|value| value.try_into().ok())
}

fn get_text_job(automation: &UIAutomation, args: Vec<JobArgument>) -> JobResult {
    let Some(JobArgument::Window(window)) = args.first() else {
        return JobResult::Err;
    };
    let Ok(element) = text_element_for_window(automation, *window, None) else {
        return JobResult::Err;
    };

    let Ok(text) = get_text(&element) else {
        return JobResult::Err;
    };

    remember_text_element(&text, &element);

    JobResult::String(text)
}

thread_local! {
    /// The element the last read came from, and the text it held.
    ///
    /// Finding a field again by searching the window for a text provider whose contents match is
    /// slow: every provider in the tree has to be read. The jobs all run on the same worker
    /// thread, so the element from the read that produced a lint can simply be kept and used when
    /// that lint is applied, which is both faster and more certain to be the same field.
    static LAST_TEXT_ELEMENT: RefCell<Option<(String, UIElement)>> = const { RefCell::new(None) };
}

fn remember_text_element(text: &str, element: &UIElement) {
    LAST_TEXT_ELEMENT.with(|cell| {
        *cell.borrow_mut() = Some((text.to_string(), element.clone()));
    });
}

/// Returns the remembered element when it still holds `expected_text`.
fn remembered_text_element(expected_text: &str) -> Option<UIElement> {
    LAST_TEXT_ELEMENT.with(|cell| {
        let cached = cell.borrow();
        let (text, element) = cached.as_ref()?;

        if text != expected_text {
            return None;
        }

        // The app may have closed or replaced the field since, so confirm it still reads back the
        // same before handing it to an edit.
        match get_text(element) {
            Ok(current) if current == expected_text => Some(element.clone()),
            _ => None,
        }
    })
}

use std::{ffi::c_void, mem::size_of};

use uiautomation::{Error, Result};
use windows::Win32::System::{
    Com::SAFEARRAY,
    Ole::{
        SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement, SafeArrayGetElemsize,
        SafeArrayGetLBound, SafeArrayGetUBound,
    },
};

struct OwnedSafeArray(*mut SAFEARRAY);

impl Drop for OwnedSafeArray {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = SafeArrayDestroy(self.0);
            }
        }
    }
}

/// Builds a text range covering `len` characters starting at `start` in the element's text.
///
/// Both the bounding boxes drawn for a lint and the selection used to replace it come from here,
/// so a highlight and the text an applied suggestion overwrites are always the same characters.
fn text_range_for_span(element: &UIElement, start: i32, len: i32) -> Result<UITextRange> {
    let pattern: UITextPattern = element.get_pattern()?;
    let range = pattern.get_document_range()?;

    range.move_endpoint_by_range(
        TextPatternRangeEndpoint::End,
        &range,
        TextPatternRangeEndpoint::Start,
    )?;

    range.move_endpoint_by_unit(TextPatternRangeEndpoint::Start, TextUnit::Character, start)?;

    range.move_endpoint_by_range(
        TextPatternRangeEndpoint::End,
        &range,
        TextPatternRangeEndpoint::Start,
    )?;

    range.move_endpoint_by_unit(TextPatternRangeEndpoint::End, TextUnit::Character, len)?;

    Ok(range)
}

/// Offsets worth trying for `span` besides its plain character count.
///
/// A span counts characters in the text Harper linted, but the accessibility API moves endpoints
/// in units of its own, and the two do not always agree. A line break the app stores as `\r\n` is
/// commonly moved over as a single unit, and anything outside the basic multilingual plane counts
/// twice. Neither disagreement is reported: the range builds happily and simply lands on the wrong
/// characters. Downstream that shows up as a lint that is found and never drawn, or a fix that
/// selects the wrong words and is refused.
fn alternative_spans(text: &str, span: Span<char>) -> Vec<(i32, i32)> {
    let characters: Vec<char> = text.chars().collect();
    let end = span.end.min(characters.len());
    let start = span.start.min(end);
    let (before, within) = (&characters[..start], &characters[start..end]);

    let utf16_units = |slice: &[char]| {
        slice
            .iter()
            .map(|character| character.len_utf16())
            .sum::<usize>() as i32
    };
    let paired_breaks = |slice: &[char]| {
        slice
            .windows(2)
            .filter(|pair| pair[0] == '\r' && pair[1] == '\n')
            .count() as i32
    };

    let plain = (start as i32, (end - start) as i32);

    [
        (
            plain.0 - paired_breaks(before),
            plain.1 - paired_breaks(within),
        ),
        (utf16_units(before), utf16_units(within)),
    ]
    .into_iter()
    .filter(|candidate| *candidate != plain && candidate.0 >= 0 && candidate.1 > 0)
    .collect()
}

/// Whether `range` holds exactly the text it was built to cover.
fn range_covers(range: &UITextRange, written: &str) -> bool {
    range
        .get_text(-1)
        .is_ok_and(|found| found.trim_end_matches('\r') == written)
}

thread_local! {
    /// The last text reported as re-anchored or unplaced, so neither is said every frame.
    static LAST_REPORTED_SPAN: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn report_once(written: &str, what: &str) {
    LAST_REPORTED_SPAN.with(|last| {
        let mut last = last.borrow_mut();

        if last.as_deref() == Some(written) {
            return;
        }

        *last = Some(written.to_string());

        eprintln!("{what}: {written:?}");
    });
}

/// Builds the range for `span` and checks it covers the text it is meant to, re-anchoring if not.
///
/// Used where a wrong range is worse than a slow one: an applied fix overwrites whatever the range
/// holds, so it is verified before anything is typed.
fn matching_text_range(element: &UIElement, text: &str, span: Span<char>) -> Result<UITextRange> {
    let plain = text_range_for_span(element, span.start as i32, span.len() as i32)?;
    let written: String = text.chars().skip(span.start).take(span.len()).collect();

    if range_covers(&plain, &written) {
        return Ok(plain);
    }

    for (start, len) in alternative_spans(text, span) {
        let range = text_range_for_span(element, start, len)?;

        if range_covers(&range, &written) {
            report_once(
                &written,
                "The app counts characters differently, re-anchored",
            );

            return Ok(range);
        }
    }

    Ok(plain)
}

fn bounding_rectangles_for_span(
    element: &UIElement,
    text: &str,
    span: Span<char>,
) -> Result<Vec<(f64, f64, f64, f64)>> {
    // The plain offsets are tried first and on their own, because they are nearly always right and
    // every extra question costs time on the focused app's own UI thread.
    let plain = text_range_for_span(element, span.start as i32, span.len() as i32)?;
    let found = rectangles_of(&plain)?;

    if !found.is_empty() {
        return Ok(found);
    }

    let written: String = text.chars().skip(span.start).take(span.len()).collect();

    for (start, len) in alternative_spans(text, span) {
        let range = text_range_for_span(element, start, len)?;

        if !range_covers(&range, &written) {
            continue;
        }

        let found = rectangles_of(&range)?;

        if !found.is_empty() {
            report_once(
                &written,
                "The app counts characters differently, re-anchored",
            );

            return Ok(found);
        }
    }

    // Nothing left to try. Scrolled out of view is a legitimate reason for this, and so is a
    // provider that will not place its own text; either way the underline cannot be drawn, so say
    // so rather than letting the lint disappear without a word.
    report_once(&written, "The app gave nowhere to draw");

    Ok(Vec::new())
}

fn rectangles_of(range: &UITextRange) -> Result<Vec<(f64, f64, f64, f64)>> {
    let raw: &IUIAutomationTextRange = range.as_ref();
    let array = OwnedSafeArray(unsafe { raw.GetBoundingRectangles()? });

    if array.0.is_null() {
        return Ok(Vec::new());
    }

    let dim = unsafe { SafeArrayGetDim(array.0) };

    if dim != 1 {
        return Err(Error::new(
            uiautomation::errors::ERR_FORMAT,
            "bounding rectangles SAFEARRAY is not one-dimensional",
        ));
    }

    let elem_size = unsafe { SafeArrayGetElemsize(array.0) };

    if elem_size as usize != size_of::<f64>() {
        return Err(Error::new(
            uiautomation::errors::ERR_FORMAT,
            "bounding rectangles SAFEARRAY does not contain f64-sized elements",
        ));
    }

    let lower = unsafe { SafeArrayGetLBound(array.0, 1)? };
    let upper = unsafe { SafeArrayGetUBound(array.0, 1)? };

    if upper < lower {
        return Ok(Vec::new());
    }

    let count = usize::try_from(i64::from(upper) - i64::from(lower) + 1)
        .map_err(|_| Error::new(uiautomation::errors::ERR_FORMAT, "SAFEARRAY is too large"))?;

    if count % 4 != 0 {
        return Err(Error::new(
            uiautomation::errors::ERR_FORMAT,
            "bounding rectangles SAFEARRAY length is not divisible by four",
        ));
    }

    let mut result = Vec::with_capacity(count / 4);

    for rect in 0..count / 4 {
        let mut values = [0.0_f64; 4];

        for (component, value) in values.iter_mut().enumerate() {
            let offset = rect
                .checked_mul(4)
                .and_then(|n| n.checked_add(component))
                .ok_or_else(|| {
                    Error::new(uiautomation::errors::ERR_FORMAT, "SAFEARRAY index overflow")
                })?;

            let index = i64::from(lower)
                .checked_add(i64::try_from(offset).map_err(|_| {
                    Error::new(uiautomation::errors::ERR_FORMAT, "SAFEARRAY index overflow")
                })?)
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| {
                    Error::new(uiautomation::errors::ERR_FORMAT, "SAFEARRAY index overflow")
                })?;

            unsafe {
                SafeArrayGetElement(array.0, &index, value as *mut f64 as *mut c_void)?;
            }
        }

        result.push((values[0], values[1], values[2], values[3]));
    }

    Ok(result)
}

fn get_bounding_rect_job(automation: &UIAutomation, arguments: Vec<JobArgument>) -> JobResult {
    let Some(JobArgument::Window(window)) = arguments.first() else {
        return JobResult::Err;
    };
    let expected_text = match arguments.get(1) {
        Some(JobArgument::Text(text)) => text.clone(),
        _ => return JobResult::Err,
    };
    let Ok(text_element) = text_element_for_window(automation, *window, Some(&expected_text))
    else {
        return JobResult::Err;
    };

    let effective_monitor_scale = get_focused_monitor_scale();

    let mut rects = Vec::with_capacity(arguments.len().saturating_sub(2));

    for span in arguments.into_iter().skip(2) {
        let span = span.expect_span();

        let Ok(found_rects) = bounding_rectangles_for_span(&text_element, &expected_text, span)
        else {
            return JobResult::Err;
        };

        rects.push(
            found_rects
                .iter()
                .map(|(x, y, w, h)| {
                    Rect::new(
                        *x / effective_monitor_scale,
                        *y / effective_monitor_scale,
                        *w / effective_monitor_scale,
                        *h / effective_monitor_scale,
                    )
                })
                .collect(),
        );
    }

    JobResult::GroupedRects(rects)
}

/// Reads what the user has selected, as an offset into the field's text plus where it starts on
/// screen.
///
/// The offset is worked out by asking for the text that runs from the start of the field up to the
/// selection and counting it, because the accessibility API compares range endpoints but will not
/// say how far apart they are.
fn get_selection_job(automation: &UIAutomation, arguments: Vec<JobArgument>) -> JobResult {
    let Some(JobArgument::Window(window)) = arguments.first() else {
        return JobResult::Err;
    };

    let Ok(element) = text_element_for_window(automation, *window, None) else {
        return JobResult::Selection(None);
    };

    let Ok(pattern) = element.get_pattern::<UITextPattern>() else {
        return JobResult::Selection(None);
    };

    let Ok(ranges) = pattern.get_selection() else {
        return JobResult::Selection(None);
    };

    let Some(range) = ranges.first() else {
        return JobResult::Selection(None);
    };

    let Ok(selected) = range.get_text(-1) else {
        return JobResult::Selection(None);
    };

    let len = selected.chars().count();

    // A caret is a selection of nothing, which is what a field reports whenever it simply has
    // focus. There is nothing to review until the user has actually marked some text.
    if len == 0 {
        return JobResult::Selection(None);
    }

    let Ok(before) = pattern.get_document_range() else {
        return JobResult::Selection(None);
    };

    if before
        .move_endpoint_by_range(
            TextPatternRangeEndpoint::End,
            range,
            TextPatternRangeEndpoint::Start,
        )
        .is_err()
    {
        return JobResult::Selection(None);
    }

    let Ok(prefix) = before.get_text(-1) else {
        return JobResult::Selection(None);
    };

    let start = prefix.chars().count();

    // The selection already has a range of its own, so its position is asked for directly rather
    // than rebuilt from offsets that the app may count differently.
    let Ok(rects) = rectangles_of(range) else {
        return JobResult::Selection(None);
    };

    let Some((x, y, _, height)) = rects.first().copied() else {
        return JobResult::Selection(None);
    };

    let scale = get_focused_monitor_scale();

    JobResult::Selection(Some(Selection {
        start,
        len,
        anchor: Rect::new(x / scale, y / scale, 0.0, height / scale),
    }))
}

fn focused_window() -> Option<(isize, u32)> {
    let hwnd: HWND = unsafe { GetForegroundWindow() };

    if hwnd.0.is_null() {
        return None;
    }

    let mut process_id = 0;

    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    }

    Some((hwnd.0 as isize, process_id))
}

#[cfg(test)]
mod tests {
    use super::alternative_spans;
    use harper_core::Span;

    #[test]
    fn plain_text_needs_no_second_guess() {
        assert!(alternative_spans("the dont there", Span::new(4, 8)).is_empty());
    }

    #[test]
    fn offers_the_offsets_an_app_that_pairs_line_breaks_would_use() {
        // Two line breaks before the mistake, so an app that moves over each pair as one character
        // is two characters ahead of where the span says the word is.
        let text = "one\r\ntwo\r\nthree dont";
        let span = Span::new(16, 20);

        assert_eq!(alternative_spans(text, span).first(), Some(&(14, 4)));
    }

    #[test]
    fn offers_the_offsets_an_app_counting_in_utf16_would_use() {
        // The emoji is one character here and two units there.
        let text = "🙂 dont";
        let span = Span::new(2, 6);

        assert!(alternative_spans(text, span).contains(&(3, 4)));
    }
}
