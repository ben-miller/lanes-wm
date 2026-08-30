use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

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

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Monitors => list_monitors(),
        Command::Place { app, monitor, position } => place(&app, &monitor, position),
        Command::Apply => apply(),
    }
}

fn list_monitors() {
    todo!("enumerate attached monitors via CoreGraphics (CGGetActiveDisplayList + CGDisplayCreateUUIDFromDisplayID) or NSScreen, print as JSON")
}

fn place(app: &str, monitor: &str, position: Position) {
    let _ = (app, monitor, position);
    todo!("resolve `app` to a running process's main window via AXUIElement, set its frame to `position` within `monitor`'s bounds")
}

fn apply() {
    let _requests: Vec<PlacementRequest> = todo!("read a JSON array of PlacementRequest from stdin");
    #[allow(unreachable_code)]
    {
        let _results: Vec<PlacementResult> = Vec::new();
        todo!("call place() for each request, collect PlacementResult, print JSON array to stdout")
    }
}
