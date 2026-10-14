use accessibility_sys::{
    kAXErrorFailure, kAXErrorSuccess, kAXFocusedWindowAttribute, kAXMinimizedAttribute,
    kAXPositionAttribute, kAXSizeAttribute, kAXTitleAttribute, kAXTrustedCheckOptionPrompt,
    kAXValueTypeCGPoint, kAXValueTypeCGSize, kAXWindowsAttribute, pid_t, AXError,
    AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElementCopyAttributeValue,
    AXUIElementCreateApplication, AXUIElementRef, AXUIElementSetAttributeValue, AXValueCreate,
};
use clap::{Parser, Subcommand, ValueEnum};
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation::uuid::{CFUUID, CFUUIDRef};
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation_sys::base::{CFRelease, CFRetain, CFTypeRef};
use core_foundation_sys::string::CFStringRef;
use core_graphics::display::{CGDirectDisplayID, CGDisplay};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;

// Not wrapped by the `core-graphics` crate. Since macOS 10.13 this symbol
// lives in ColorSync (it moved out of CoreGraphics/ApplicationServices).
#[link(name = "ColorSync", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayCreateUUIDFromDisplayID(display: CGDirectDisplayID) -> CFUUIDRef;
}

// Returns None if the display has no stable UUID right now (Apple's API can
// return NULL here, e.g. for a mirrored or just-detached display).
fn display_uuid(id: CGDirectDisplayID) -> Option<uuid::Uuid> {
    let cf_ref = unsafe { CGDisplayCreateUUIDFromDisplayID(id) };
    if cf_ref.is_null() {
        return None;
    }
    let cf_uuid = unsafe { CFUUID::wrap_under_create_rule(cf_ref) };
    Some(cf_uuid.into())
}

/// A single AXUIElement (an app, or one of its windows), owning one CF
/// reference to it - released on drop.
struct AxElement(AXUIElementRef);

impl AxElement {
    fn for_pid(pid: pid_t) -> Self {
        AxElement(unsafe { AXUIElementCreateApplication(pid) })
    }

    fn copy_attribute(&self, attribute: &str) -> Result<CFTypeRef, AXError> {
        let attr = CFString::new(attribute);
        let mut value: CFTypeRef = std::ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValue(self.0, attr.as_concrete_TypeRef(), &mut value)
        };
        if err == kAXErrorSuccess {
            Ok(value)
        } else {
            Err(err)
        }
    }

    /// This app's windows, per kAXWindowsAttribute. Empty if the app has no
    /// windows or the attribute can't be read (e.g. the app just quit).
    fn windows(&self) -> Vec<AxElement> {
        let Ok(value) = self.copy_attribute(kAXWindowsAttribute) else {
            return Vec::new();
        };
        let array_ref = value as CFArrayRef;
        let count = unsafe { CFArrayGetCount(array_ref) };
        let mut windows = Vec::with_capacity(count.max(0) as usize);
        for i in 0..count {
            let item = unsafe { CFArrayGetValueAtIndex(array_ref, i) };
            // CFArrayGetValueAtIndex follows the Get Rule (no implicit
            // retain), so bump the refcount before AxElement's Drop
            // releases it.
            unsafe { CFRetain(item) };
            windows.push(AxElement(item as AXUIElementRef));
        }
        unsafe { CFRelease(value) };
        windows
    }

    /// This app's currently-focused window, per kAXFocusedWindowAttribute -
    /// the app's own notion of which window is active, no title matching
    /// involved. None if the app has no focused window (e.g. it has no
    /// windows at all, or the attribute isn't readable).
    fn focused_window(&self) -> Option<AxElement> {
        let value = self.copy_attribute(kAXFocusedWindowAttribute).ok()?;
        if value.is_null() {
            return None;
        }
        Some(AxElement(value as AXUIElementRef))
    }

    fn title(&self) -> Option<String> {
        let value = self.copy_attribute(kAXTitleAttribute).ok()?;
        if value.is_null() {
            return None;
        }
        let cf_string =
            unsafe { CFString::wrap_under_create_rule(value as CFStringRef) };
        Some(cf_string.to_string())
    }

    fn set_frame(&self, frame: CGRect) -> Result<(), String> {
        self.set_ax_value(kAXPositionAttribute, kAXValueTypeCGPoint, &frame.origin)
            .map_err(|e| format!("failed to set position: {}", accessibility_sys::error_string(e)))?;
        self.set_ax_value(kAXSizeAttribute, kAXValueTypeCGSize, &frame.size)
            .map_err(|e| format!("failed to set size: {}", accessibility_sys::error_string(e)))?;
        Ok(())
    }

    /// Set kAXMinimizedAttribute. Unlike position/size, this is a plain
    /// boolean attribute - no AXValueCreate wrapping (that's only for the
    /// geometry/range AXValue types position/size use), just a CFBoolean
    /// passed directly to AXUIElementSetAttributeValue.
    fn set_minimized(&self, minimized: bool) -> Result<(), String> {
        let attr = CFString::new(kAXMinimizedAttribute);
        let value = if minimized { CFBoolean::true_value() } else { CFBoolean::false_value() };
        let err = unsafe {
            AXUIElementSetAttributeValue(self.0, attr.as_concrete_TypeRef(), value.as_concrete_TypeRef() as CFTypeRef)
        };
        if err == kAXErrorSuccess {
            Ok(())
        } else {
            Err(format!("failed to set minimized: {}", accessibility_sys::error_string(err)))
        }
    }

    fn set_ax_value<T>(&self, attribute: &str, ax_type: u32, value: &T) -> Result<(), AXError> {
        let attr = CFString::new(attribute);
        let ax_value = unsafe { AXValueCreate(ax_type, value as *const T as *const c_void) };
        if ax_value.is_null() {
            return Err(kAXErrorFailure); // AXValueCreate itself gives no error code on failure.
        }
        let err = unsafe {
            AXUIElementSetAttributeValue(self.0, attr.as_concrete_TypeRef(), ax_value as CFTypeRef)
        };
        unsafe { CFRelease(ax_value as CFTypeRef) };
        if err == kAXErrorSuccess {
            Ok(())
        } else {
            Err(err)
        }
    }
}

impl Drop for AxElement {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0 as CFTypeRef) };
    }
}

/// Resolve a bundle ID to the pids of its currently running instances.
fn pids_for_bundle_id(bundle_id: &str) -> Vec<pid_t> {
    let workspace = NSWorkspace::sharedWorkspace();
    let running = workspace.runningApplications();
    running
        .iter()
        .filter(|app| {
            app.bundleIdentifier()
                .is_some_and(|b| b.to_string() == bundle_id)
        })
        .map(|app| app.processIdentifier())
        .collect()
}

/// Bring a specific process to the front, by PID rather than bundle ID -
/// `open -a`/`open -b` can't target one specific instance among several
/// sharing a bundle ID (e.g. Firefox running multiple profiles), and
/// `NSRunningApplication` is keyed by PID regardless of that. Placement
/// alone (setFrame) doesn't affect z-order, so two windows placed at the
/// same coordinates across different calls - the common case for a
/// per-lane "this app always goes here" target - need this to make sure
/// the one you actually want visible ends up on top.
fn raise_pid(pid: pid_t) -> Result<(), String> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .ok_or_else(|| format!("no running application with pid {pid}"))?;
    // ActivateIgnoringOtherApps is deprecated (macOS 14+) and documented as
    // having no effect - plain activation (empty options) is what actually
    // raises the app now.
    let activated = app.activateWithOptions(NSApplicationActivationOptions::empty());
    if activated {
        Ok(())
    } else {
        Err(format!("failed to activate pid {pid}"))
    }
}

/// Which process(es) a placement targets.
#[derive(Debug)]
enum AppTarget {
    /// Every process matching this bundle ID - the common case.
    BundleId(String),
    /// One specific process, by PID. For apps that run as several
    /// independent instances sharing one bundle ID (e.g. Firefox launched
    /// with `--profile <path>` for multiple profiles), bundle-ID matching
    /// alone can't tell them apart - the caller is expected to have
    /// already resolved which instance it means (e.g. by reading its
    /// launch arguments) before calling this.
    Pid(pid_t),
}

impl AppTarget {
    fn pids(&self) -> Vec<pid_t> {
        match self {
            AppTarget::BundleId(id) => pids_for_bundle_id(id),
            AppTarget::Pid(pid) => vec![*pid],
        }
    }

    /// Human-readable label for error messages and PlacementResult's `app`
    /// field - a bundle ID, or "pid <N>" when targeted that way.
    fn label(&self) -> String {
        match self {
            AppTarget::BundleId(id) => id.clone(),
            AppTarget::Pid(pid) => format!("pid {pid}"),
        }
    }
}

/// Build an AppTarget from the (app, pid) pair shared by the CLI flags and
/// PlacementRequest's JSON fields. Errors if both or neither are set - same
/// mutual-exclusivity `place`'s clap flags enforce for the CLI case, needed
/// again here since a JSON batch request bypasses clap's `conflicts_with`.
fn app_target_from(app: Option<String>, pid: Option<u32>) -> Result<AppTarget, String> {
    match (app, pid) {
        (Some(a), None) => Ok(AppTarget::BundleId(a)),
        (None, Some(p)) => Ok(AppTarget::Pid(p as pid_t)),
        (Some(_), Some(_)) => Err("app and pid are mutually exclusive".to_string()),
        (None, None) => Err("one of app or pid is required".to_string()),
    }
}

/// How to pick which of an app's windows a placement applies to.
#[derive(Debug)]
enum Selector {
    /// Only valid when the app has exactly one window.
    Any,
    /// Regex matched against kAXTitleAttribute.
    Title(String),
    /// Whichever window is currently focused (kAXFocusedWindowAttribute),
    /// no title involved. For apps whose titles reflect content rather than
    /// identity (e.g. Obsidian's title is "<note> - <vault> - Obsidian") -
    /// the caller is expected to have already made the right window
    /// frontmost (e.g. via an app-specific URI/CLI) before calling this.
    Focused,
}

/// Find the single window matching `target` and `selector`. Deliberately
/// strict: zero or multiple matches is an error, not a guess - see
/// README's "fail loudly" philosophy for monitor matching, which applies
/// just as much to windows.
/// Returns the matched window along with the PID it belongs to - needed
/// for `--raise`, which activates a specific process rather than going
/// through a bundle-ID-based (and thus, for a multi-instance app,
/// ambiguous) app-activation call.
fn find_window(target: &AppTarget, selector: &Selector) -> Result<(pid_t, AxElement), String> {
    log::debug!("AXIsProcessTrusted: {}", unsafe { AXIsProcessTrusted() });
    let pids = target.pids();
    let label = target.label();
    log::debug!("pids for {label}: {pids:?}");
    if pids.is_empty() {
        return Err(format!("app not running: {label}"));
    }

    if let Selector::Focused = selector {
        let matched: Vec<(pid_t, AxElement)> = pids
            .into_iter()
            .filter_map(|pid| AxElement::for_pid(pid).focused_window().map(|w| (pid, w)))
            .collect();
        return match matched.len() {
            0 => Err(format!("no focused window (app: {label})")),
            1 => Ok(matched.into_iter().next().unwrap()),
            n => Err(format!(
                "ambiguous: {n} instances of {label} each have a focused window"
            )),
        };
    }

    let windows: Vec<(pid_t, AxElement)> = pids
        .into_iter()
        .flat_map(|pid| {
            let ws = AxElement::for_pid(pid).windows();
            log::debug!("pid {pid} has {} window(s)", ws.len());
            ws.into_iter().map(move |w| (pid, w))
        })
        .collect();

    let matched: Vec<(pid_t, AxElement)> = match selector {
        Selector::Any => windows,
        Selector::Title(pattern) => {
            let re = regex::Regex::new(pattern).map_err(|e| format!("invalid title pattern {pattern:?}: {e}"))?;
            windows
                .into_iter()
                .filter(|(_, w)| w.title().is_some_and(|t| re.is_match(&t)))
                .collect()
        }
        Selector::Focused => unreachable!("handled above"),
    };

    match matched.len() {
        0 => Err(format!("no window matched (app: {label}, selector: {selector:?})")),
        1 => Ok(matched.into_iter().next().unwrap()),
        n => Err(format!(
            "ambiguous: {n} windows matched (app: {label}, selector: {selector:?}) - narrow the title pattern"
        )),
    }
}

fn monitor_bounds(uuid_str: &str) -> Result<CGRect, String> {
    let ids = CGDisplay::active_displays().map_err(|_| "CGGetActiveDisplayList failed".to_string())?;
    for id in ids {
        if display_uuid(id).is_some_and(|u| u.to_string().eq_ignore_ascii_case(uuid_str)) {
            return Ok(CGDisplay::new(id).bounds());
        }
    }
    Err(format!("monitor not found: {uuid_str}"))
}

/// Divide a monitor into a `cols` x `rows` grid (CSS Grid's model: N tracks,
/// placement by 1-based span of track indices) and compute the rect a given
/// column/row span occupies. Generalizes the five presets, which are just
/// well-known spans of a 1x1 or 2x1/1x2 grid - see `Preset::to_grid`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Grid {
    #[serde(default = "one")]
    cols: u32,
    #[serde(default = "unit_span")]
    col: (u32, u32),
    #[serde(default = "one")]
    rows: u32,
    #[serde(default = "unit_span")]
    row: (u32, u32),
}

fn one() -> u32 {
    1
}
fn unit_span() -> (u32, u32) {
    (1, 1)
}

impl Grid {
    fn validate(&self) -> Result<(), String> {
        if self.cols == 0 || self.rows == 0 {
            return Err("grid cols/rows must be >= 1".to_string());
        }
        let (c0, c1) = self.col;
        let (r0, r1) = self.row;
        if c0 == 0 || r0 == 0 {
            return Err("grid col/row spans are 1-based - use 1, not 0".to_string());
        }
        if c0 > c1 || c1 > self.cols {
            return Err(format!("col span {c0}-{c1} out of range for {} column(s)", self.cols));
        }
        if r0 > r1 || r1 > self.rows {
            return Err(format!("row span {r0}-{r1} out of range for {} row(s)", self.rows));
        }
        Ok(())
    }

    fn frame(&self, bounds: CGRect) -> CGRect {
        let CGRect { origin, size } = bounds;
        let (x, y, w, h) = (origin.x, origin.y, size.width, size.height);
        let col_w = w / self.cols as f64;
        let row_h = h / self.rows as f64;
        let (c0, c1) = self.col;
        let (r0, r1) = self.row;
        let px = x + (c0 - 1) as f64 * col_w;
        let py = y + (r0 - 1) as f64 * row_h;
        let pw = (c1 - c0 + 1) as f64 * col_w;
        let ph = (r1 - r0 + 1) as f64 * row_h;
        CGRect::new(&CGPoint::new(px, py), &CGSize::new(pw, ph))
    }
}

/// Named shorthand for common grid spans, borrowed from Rectangle's
/// vocabulary. This is the only form `place`'s CLI flag accepts (it's a
/// manual-testing convenience, not the primary interface); `apply`'s JSON
/// contract accepts a `Grid` directly too, via `Position`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum Preset {
    Full,
    LeftHalf,
    RightHalf,
    TopHalf,
    BottomHalf,
}

impl Preset {
    fn to_grid(self) -> Grid {
        match self {
            Preset::Full => Grid { cols: 1, col: (1, 1), rows: 1, row: (1, 1) },
            Preset::LeftHalf => Grid { cols: 2, col: (1, 1), rows: 1, row: (1, 1) },
            Preset::RightHalf => Grid { cols: 2, col: (2, 2), rows: 1, row: (1, 1) },
            Preset::TopHalf => Grid { cols: 1, col: (1, 1), rows: 2, row: (1, 1) },
            Preset::BottomHalf => Grid { cols: 1, col: (1, 1), rows: 2, row: (2, 2) },
        }
    }
}

/// Either a named preset or a raw grid span - accepted interchangeably
/// wherever a placement's position is given in JSON (`"full"` or
/// `{"cols": 3, "col": [1, 2]}`).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum Position {
    Preset(Preset),
    Grid(Grid),
}

impl Position {
    fn to_grid(self) -> Grid {
        match self {
            Position::Preset(p) => p.to_grid(),
            Position::Grid(g) => g,
        }
    }
}

fn position_frame(bounds: CGRect, position: Position) -> Result<CGRect, String> {
    let grid = position.to_grid();
    grid.validate()?;
    Ok(grid.frame(bounds))
}

/// lanes-wm: a standalone, lane-agnostic window-placement primitive for macOS.
///
/// Knows nothing about Lanes' own concepts (lanes, monitor handles, topology
/// gates) - callers resolve those down to concrete app bundle IDs and
/// monitor UUIDs before invoking this. See README.md for the design
/// rationale (why a CLI and not a daemon, why direct Accessibility API
/// calls and not Hammerspoon).
#[derive(Parser)]
#[command(name = "lanes-wm", about = "Standalone window-placement primitive for macOS")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Emit single-line JSON instead of pretty-printed (for piping to
    /// another program, e.g. lanes-cli). Doesn't change what's valid to
    /// parse, just the whitespace.
    #[arg(long, global = true)]
    compact: bool,
}

#[derive(Subcommand)]
enum Command {
    /// List monitors currently attached, as JSON: [{uuid, name, frame}]
    Monitors,

    /// Place a single app's window on a monitor. A convenience for manual
    /// testing - `apply` is the primary interface, since a real lane switch
    /// always moves several windows at once and should do it in one call.
    Place {
        /// Bundle identifier of the target app, e.g. com.github.wez.wezterm.
        /// Conflicts with --pid.
        #[arg(long, conflicts_with = "pid")]
        app: Option<String>,
        /// Target one specific process by PID instead of a bundle ID - for
        /// apps that run as several independent instances sharing one
        /// bundle ID (e.g. Firefox with multiple profiles), where the
        /// caller has already resolved which instance it means. Conflicts
        /// with --app; exactly one of the two is required.
        #[arg(long)]
        pid: Option<u32>,
        /// Target monitor's CGDisplay UUID (see `lanes-wm monitors`)
        #[arg(long)]
        monitor: String,
        /// Placement preset
        #[arg(long, value_enum)]
        position: Preset,
        /// Regex matched against window titles, to pick one window when the
        /// app has more than one. Required if the app has more than one
        /// window open; omit only when it has exactly one. Conflicts with
        /// --focused.
        #[arg(long, conflicts_with = "focused")]
        title: Option<String>,
        /// Target whichever window is currently focused for this app,
        /// instead of matching by title. For apps whose titles reflect
        /// content rather than identity (e.g. Obsidian) - the caller is
        /// expected to have already made the right window frontmost (e.g.
        /// via an app-specific URI/CLI) before calling this. Conflicts
        /// with --title.
        #[arg(long)]
        focused: bool,
        /// Also bring the matched window's process to the front
        /// (NSRunningApplication, by PID - not `open`, which can't target
        /// one specific instance among several sharing a bundle ID).
        /// Placement alone doesn't affect z-order, so if this app's window
        /// is placed at the same coordinates on every call (the common
        /// case for a per-lane "this always goes here" target), whichever
        /// one was already on top stays there unless raised explicitly.
        #[arg(long)]
        raise: bool,
    },

    /// Apply a batch of placements read as JSON from stdin, one result per
    /// input written to stdout - a partial failure doesn't abort the batch.
    Apply,

    /// Trigger macOS's native Accessibility permission prompt for this
    /// binary. Manually adding a raw CLI executable via the `+` button in
    /// System Settings > Privacy & Security > Accessibility does not
    /// reliably register - this does it the way macOS expects.
    RequestAccess,

    /// Report Accessibility trust status with no side effects, exit code
    /// only (0 trusted, 1 not) - unlike `request-access`, never triggers
    /// the system permission prompt. Meant for a diagnostic (e.g. lanes'
    /// `doctor`) to check status on every run without ever popping a
    /// dialog just from running a health check.
    Trusted,
}

#[derive(Deserialize)]
struct PlacementRequest {
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    /// `monitor`/`position` are optional as a pair - a request with
    /// `minimized: true` and neither doesn't place the window at all, it
    /// only minimizes it. A request setting one without the other is a
    /// per-item failure, same class of thing as `app`/`pid` both-or-neither.
    #[serde(default)]
    monitor: Option<String>,
    #[serde(default)]
    position: Option<Position>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    raise: bool,
    /// Minimize (true) or de-minimize (false) the matched window via
    /// kAXMinimizedAttribute, instead of (not in addition to - raising a
    /// window you just asked to minimize makes no sense) moving/resizing
    /// it. Mutually exclusive with `monitor`/`position` being set.
    #[serde(default)]
    minimized: Option<bool>,
}

impl PlacementRequest {
    fn selector(&self) -> Result<Selector, String> {
        selector_from(self.title.clone(), self.focused)
    }

    fn app_target(&self) -> Result<AppTarget, String> {
        app_target_from(self.app.clone(), self.pid)
    }

    /// Label for PlacementResult's `app` field - the bundle ID if that's
    /// how this request targeted its app, else "pid <N>". Computed even
    /// when app_target() itself would error (both or neither given), so
    /// the per-item result still has something meaningful to show.
    fn label(&self) -> String {
        match (&self.app, self.pid) {
            (Some(a), _) => a.clone(),
            (None, Some(p)) => format!("pid {p}"),
            (None, None) => "?".to_string(),
        }
    }
}

/// Build a Selector from the (title, focused) pair shared by the CLI flags
/// and PlacementRequest's JSON fields. Errors if both are set - same
/// mutual-exclusivity `place`'s clap flags enforce, since a JSON batch
/// request bypasses clap's `conflicts_with`.
fn selector_from(title: Option<String>, focused: bool) -> Result<Selector, String> {
    match (title, focused) {
        (Some(_), true) => Err("title and focused are mutually exclusive".to_string()),
        (Some(t), false) => Ok(Selector::Title(t)),
        (None, true) => Ok(Selector::Focused),
        (None, false) => Ok(Selector::Any),
    }
}

#[derive(Serialize)]
struct PlacementResult {
    app: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct Frame {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[derive(Serialize)]
struct MonitorInfo {
    uuid: String,
    name: String,
    frame: Frame,
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();
    match cli.command {
        Command::Monitors => list_monitors(cli.compact),
        Command::Place { app, pid, monitor, position, title, focused, raise } => {
            let target = app_target_from(app, pid).unwrap_or_else(|e| {
                eprintln!("lanes-wm: {e}");
                std::process::exit(1);
            });
            let selector = selector_from(title, focused).unwrap_or_else(|e| {
                eprintln!("lanes-wm: {e}");
                std::process::exit(1);
            });
            if let Err(e) = place(&target, &monitor, Position::Preset(position), &selector, raise) {
                eprintln!("lanes-wm: {e}");
                std::process::exit(1);
            }
        }
        Command::Apply => apply(cli.compact),
        Command::RequestAccess => request_access(),
        Command::Trusted => {
            if !unsafe { AXIsProcessTrusted() } {
                std::process::exit(1);
            }
        }
    }
}

fn request_access() {
    let prompt_key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
    let options = CFDictionary::from_CFType_pairs(&[(prompt_key, CFBoolean::true_value())]);
    let trusted = unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) };
    if trusted {
        println!("lanes-wm is already trusted for Accessibility.");
    } else {
        println!(
            "Requested Accessibility access - look for a system permission prompt (it may be behind this window), and click Allow."
        );
    }
}

fn print_json<T: Serialize>(value: &T, compact: bool) {
    let text = if compact {
        serde_json::to_string(value)
    } else {
        serde_json::to_string_pretty(value)
    };
    println!("{}", text.expect("failed to serialize JSON"));
}

fn list_monitors(compact: bool) {
    let ids = CGDisplay::active_displays().expect("CGGetActiveDisplayList failed");
    let monitors: Vec<MonitorInfo> = ids
        .into_iter()
        .filter_map(|id| {
            let Some(uuid) = display_uuid(id) else {
                // No stable UUID to match against later (mirrored/detaching
                // display) - report it and drop it rather than emitting a
                // monitor `place` could never target.
                eprintln!("lanes-wm: skipping display {id}, no UUID available");
                return None;
            };
            let display = CGDisplay::new(id);
            let bounds = display.bounds();
            // Real product names (EDID via IODisplayConnect) are unreliable
            // on Apple Silicon, so this is deliberately coarse for now -
            // enough to tell displays apart, not a marketing name.
            let name = if display.is_builtin() {
                "Built-in Display".to_string()
            } else {
                format!("External Display ({id})")
            };
            Some(MonitorInfo {
                uuid: uuid.to_string().to_uppercase(),
                name,
                frame: Frame {
                    x: bounds.origin.x,
                    y: bounds.origin.y,
                    w: bounds.size.width,
                    h: bounds.size.height,
                },
            })
        })
        .collect();
    print_json(&monitors, compact);
}

fn place(target: &AppTarget, monitor: &str, position: Position, selector: &Selector, raise: bool) -> Result<(), String> {
    log::info!(
        "placing app={} monitor={monitor} position={position:?} selector={selector:?} raise={raise}",
        target.label()
    );
    let bounds = monitor_bounds(monitor)?;
    let frame = position_frame(bounds, position)?;
    let (pid, window) = find_window(target, selector)?;
    window.set_frame(frame)?;
    if raise {
        raise_pid(pid)?;
    }
    Ok(())
}

fn apply(compact: bool) {
    let requests: Vec<PlacementRequest> = match serde_json::from_reader(std::io::stdin()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("lanes-wm: invalid JSON on stdin: {e}");
            std::process::exit(1);
        }
    };
    log::info!("applying {} placement(s)", requests.len());
    let results: Vec<PlacementResult> = requests
        .into_iter()
        .map(|req| {
            let label = req.label();
            let outcome = apply_one(&req);
            match outcome {
                Ok(()) => PlacementResult { app: label, ok: true, error: None },
                Err(e) => {
                    log::warn!("placement failed: {e}");
                    PlacementResult { app: label, ok: false, error: Some(e) }
                }
            }
        })
        .collect();
    print_json(&results, compact);
}

/// One request's worth of work: either minimize/de-minimize the matched
/// window, or place it (optionally raising) - never both, since raising a
/// window you just asked to minimize is contradictory.
fn apply_one(req: &PlacementRequest) -> Result<(), String> {
    let target = req.app_target()?;
    let selector = req.selector()?;

    if let Some(minimized) = req.minimized {
        if req.monitor.is_some() || req.position.is_some() {
            return Err("`minimized` is mutually exclusive with `monitor`/`position`".to_string());
        }
        let (_, window) = find_window(&target, &selector)?;
        log::info!("setting minimized={minimized} app={}", target.label());
        return window.set_minimized(minimized);
    }

    match (&req.monitor, req.position) {
        // Neither monitor nor position - a pure activation request, for a
        // target whose whole job is "bring the right window/profile
        // forward" with no move/resize involved (that's `raise`'s only
        // meaning here; a request with raise=false too is a no-op resolve,
        // which is on the caller, not this function's problem to reject).
        (None, None) => {
            let (pid, _window) = find_window(&target, &selector)?;
            if req.raise {
                raise_pid(pid)?;
            }
            Ok(())
        }
        (Some(monitor), Some(position)) => place(&target, monitor, position, &selector, req.raise),
        _ => Err("request must set both `monitor` and `position` together, or neither".to_string()),
    }
}
