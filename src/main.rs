use clap::{Parser, Subcommand, ValueEnum};
use core_foundation::base::TCFType;
use core_foundation::uuid::{CFUUID, CFUUIDRef};
use core_graphics::display::{CGDirectDisplayID, CGDisplay};
use serde::{Deserialize, Serialize};

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

    /// Place a single app's window on a monitor
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
    },

    /// Apply a batch of placements read as JSON from stdin, one result per
    /// input written to stdout - a partial failure doesn't abort the batch.
    Apply,
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
    let cli = Cli::parse();
    match cli.command {
        Command::Monitors => list_monitors(cli.compact),
        Command::Place { app, monitor, position } => place(&app, &monitor, position),
        Command::Apply => apply(cli.compact),
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

fn place(app: &str, monitor: &str, position: Position) {
    let _ = (app, monitor, position);
    todo!("resolve `app` to a running process's main window via AXUIElement, set its frame to `position` within `monitor`'s bounds")
}

fn apply(compact: bool) {
    let _ = compact;
    let _requests: Vec<PlacementRequest> = todo!("read a JSON array of PlacementRequest from stdin");
    #[allow(unreachable_code)]
    {
        let _results: Vec<PlacementResult> = Vec::new();
        todo!("call place() for each request, collect PlacementResult, print via print_json(&_results, compact)")
    }
}
