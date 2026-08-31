use accessibility_sys::{
    kAXErrorFailure, kAXErrorSuccess, kAXPositionAttribute, kAXSizeAttribute, kAXTitleAttribute,
    kAXTrustedCheckOptionPrompt, kAXValueTypeCGPoint, kAXValueTypeCGSize, kAXWindowsAttribute,
    pid_t, AXError, AXIsProcessTrusted, AXIsProcessTrustedWithOptions,
    AXUIElementCopyAttributeValue, AXUIElementCreateApplication, AXUIElementRef,
    AXUIElementSetAttributeValue, AXValueCreate,
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
use objc2_app_kit::NSWorkspace;
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

/// Find the single window matching `app` (and, if given, a regex `title`
/// pattern). Deliberately strict: zero or multiple matches is an error, not
/// a guess - see README's "fail loudly" philosophy for monitor matching,
/// which applies just as much to windows.
fn find_window(app: &str, title: Option<&str>) -> Result<AxElement, String> {
    log::debug!("AXIsProcessTrusted: {}", unsafe { AXIsProcessTrusted() });
    let pids = pids_for_bundle_id(app);
    log::debug!("pids for {app}: {pids:?}");
    if pids.is_empty() {
        return Err(format!("app not running: {app}"));
    }

    let windows: Vec<AxElement> = pids
        .into_iter()
        .flat_map(|pid| {
            let ws = AxElement::for_pid(pid).windows();
            log::debug!("pid {pid} has {} window(s)", ws.len());
            ws
        })
        .collect();

    let matched: Vec<AxElement> = match title {
        None => windows,
        Some(pattern) => {
            let re = regex::Regex::new(pattern).map_err(|e| format!("invalid title pattern {pattern:?}: {e}"))?;
            windows
                .into_iter()
                .filter(|w| w.title().is_some_and(|t| re.is_match(&t)))
                .collect()
        }
    };

    match matched.len() {
        0 => Err(format!("no window matched (app: {app}, title: {title:?})")),
        1 => Ok(matched.into_iter().next().unwrap()),
        n => Err(format!(
            "ambiguous: {n} windows matched (app: {app}, title: {title:?}) - narrow the title pattern"
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

fn position_frame(bounds: CGRect, position: Position) -> CGRect {
    let CGRect { origin, size } = bounds;
    let (x, y, w, h) = (origin.x, origin.y, size.width, size.height);
    let (px, py, pw, ph) = match position {
        Position::Full => (x, y, w, h),
        Position::LeftHalf => (x, y, w / 2.0, h),
        Position::RightHalf => (x + w / 2.0, y, w / 2.0, h),
        Position::TopHalf => (x, y, w, h / 2.0),
        Position::BottomHalf => (x, y + h / 2.0, w, h / 2.0),
    };
    CGRect::new(&CGPoint::new(px, py), &CGSize::new(pw, ph))
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
        /// Bundle identifier of the target app, e.g. com.github.wez.wezterm
        #[arg(long)]
        app: String,
        /// Target monitor's CGDisplay UUID (see `lanes-wm monitors`)
        #[arg(long)]
        monitor: String,
        /// Placement preset
        #[arg(long, value_enum)]
        position: Position,
        /// Regex matched against window titles, to pick one window when the
        /// app has more than one. Required if the app has more than one
        /// window open; omit only when it has exactly one.
        #[arg(long)]
        title: Option<String>,
    },

    /// Apply a batch of placements read as JSON from stdin, one result per
    /// input written to stdout - a partial failure doesn't abort the batch.
    Apply,

    /// Trigger macOS's native Accessibility permission prompt for this
    /// binary. Manually adding a raw CLI executable via the `+` button in
    /// System Settings > Privacy & Security > Accessibility does not
    /// reliably register - this does it the way macOS expects.
    RequestAccess,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum Position {
    Full,
    LeftHalf,
    RightHalf,
    TopHalf,
    BottomHalf,
}

#[derive(Deserialize)]
struct PlacementRequest {
    app: String,
    monitor: String,
    position: Position,
    #[serde(default)]
    title: Option<String>,
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
        Command::Place { app, monitor, position, title } => {
            if let Err(e) = place(&app, &monitor, position, title.as_deref()) {
                eprintln!("lanes-wm: {e}");
                std::process::exit(1);
            }
        }
        Command::Apply => apply(cli.compact),
        Command::RequestAccess => request_access(),
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

fn place(app: &str, monitor: &str, position: Position, title: Option<&str>) -> Result<(), String> {
    log::info!("placing app={app} monitor={monitor} position={position:?} title={title:?}");
    let bounds = monitor_bounds(monitor)?;
    let frame = position_frame(bounds, position);
    let window = find_window(app, title)?;
    window.set_frame(frame)
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
            let outcome = place(&req.app, &req.monitor, req.position, req.title.as_deref());
            match outcome {
                Ok(()) => PlacementResult { app: req.app, ok: true, error: None },
                Err(e) => {
                    log::warn!("placement failed: {e}");
                    PlacementResult { app: req.app, ok: false, error: Some(e) }
                }
            }
        })
        .collect();
    print_json(&results, compact);
}
