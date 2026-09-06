# lanes-wm

A standalone, lane-agnostic window-placement primitive for macOS. Given an
app and a monitor, move that app's window into a preset position on it.
Built to be called from [lanes](../lanes) on lane focus, but doesn't know
what a "lane" is.

## Status

`monitors`, `place`, and `apply` are all implemented and verified working
end-to-end against real windows (CoreGraphics for display IDs and bounds,
ColorSync's `CGDisplayCreateUUIDFromDisplayID` for UUIDs - see note below;
the Accessibility `AXUIElement` API for window resolution and placement -
see "Local dev setup" below for the one-time permission/signing steps this
requires). Not yet wired into lanes-cli.

**Monitor names are a coarse placeholder** ("Built-in Display" /
"External Display (<id>)"), not the real product name. The standard way to
get that (EDID via `IODisplayConnect`) is unreliable on Apple Silicon, so
it's deliberately skipped for now rather than shipping something that
silently returns wrong names on M-series Macs. Since placement matches by
UUID, not name, this doesn't block `place`/`apply` - it only makes `monitors`
output less human-readable. A future fix would probe `IODPDevice` or shell
out to `system_profiler SPDisplaysDataType -json`.

## Why this exists

lanes lets you switch rapidly between "lanes" - named working contexts, each
with its own terminal session, repo, and (this project's job) window
layout. `lanes focus <id>` should be able to snap a lane's apps into place
on its target monitors as part of switching.

## Design decisions and why

**A separate repo, not a module inside lanes-cli.** No Rust code is shared
between them - the interface is a CLI/JSON contract, not a linked library -
so splitting costs nothing in code-sharing terms. It also means this can be
developed independently of lanes-cli's own repo (which may have other work
in flight), and lets it stay a genuinely reusable tool with no lanes-specific
vocabulary (no "lane", no "monitor handle", no topology gate - those live in
lanes-cli, which resolves them down to concrete bundle IDs and monitor UUIDs
before calling here).

**A one-shot CLI, not a daemon.** Every operation here is stateless: given
the current hardware and a request, do a thing, exit. Nothing needs to
persist between invocations, and nothing needs to react to events it wasn't
told about (no "watch for a new window and auto-place it" - that's a
reactive rule-engine model, deliberately out of scope). The thing that *does*
need to be a persistent daemon is whatever's catching the global hotkey that
triggers a lane switch in the first place - today that's Hammerspoon, run as
its own always-running process; this tool is just a client, invoked
synchronously, the same way `wezterm cli activate-tab` is a one-shot client
to the already-running WezTerm process.

**Calls the macOS Accessibility API directly, not through Hammerspoon.**
Hammerspoon's own placement logic (`hs.screen`, `hs.application`,
`window:setFrame()`) is itself a thin Lua wrapper over
CoreGraphics/Accessibility. Routing through it would mean anyone using this
tool - including a future rewrite of the hotkey layer itself, see below -
needs Hammerspoon.app installed, running, with `hs.ipc` loaded. Calling
`AXUIElement`/CoreGraphics directly removes that dependency entirely: one
binary, one Accessibility permission grant (System Settings > Privacy &
Security > Accessibility), nothing else has to be running.
Rust fits this specific job well - CoreFoundation's retain/release memory
model maps naturally onto Rust's ownership model, which is why safe wrapper
crates exist for this (`accessibility`/`accessibility-sys`, `axuielement`) in
a way that's notably cleaner than the equivalent in a GC'd language without
ownership tracking (Go's binding for the same API leaves retain/release
bookkeeping as the caller's problem).

**Position is a closed preset enum, not a fraction DSL.** `full`,
`left-half`, `right-half`, `top-half`, `bottom-half` today, borrowed from
Rectangle's naming (a well-known, already-recognizable vocabulary), with
thirds/quarters as an anticipated but not-yet-built extension.

**Monitors are matched by exact UUID, no fallback matching.** Deliberately
strict: if a monitor's UUID drifts (reconnect, cable swap), a placement
targeting it should visibly do nothing or fail loudly, not silently guess
via a name/position fallback chain. `lanes-wm monitors` exists so you can
see the truth (what's actually attached right now) and manually update
whatever config points at a stale UUID.

## Interface

```
lanes-wm monitors
```
stdout (JSON): every monitor currently attached, nothing configured/assumed.
```json
[{"uuid": "37D8832A-...", "name": "Built-in Retina Display", "frame": {"x": 0, "y": 0, "w": 1512, "h": 982}}]
```

All commands pretty-print JSON by default; pass `--compact` for single-line
output when piping to another program, e.g. `lanes-wm monitors --compact`.

```
lanes-wm place --app <bundle-id> --monitor <uuid> --position <preset> [--title <regex> | --focused]
```
Single placement. Exit 0 on success; non-zero + stderr message on failure
(app not running, monitor not found, etc).

Two mutually exclusive, optional ways to pick which window when the app has
more than one - omit both only when the app is known to have exactly one
window:

- `--title` - a regex matched against each of the app's window titles (via
  `kAXTitleAttribute`). There's no stable window identifier in the
  Accessibility API, so title matching is the established approach (yabai
  does the same) - but it's inherently unreliable for apps whose titles
  reflect current content rather than window identity (e.g. Obsidian's
  title is `<note> - <vault> - Obsidian`, which changes every time you
  switch notes).
- `--focused` - targets whichever window is currently focused for the app
  (`kAXFocusedWindowAttribute`), no title involved at all. Meant for the
  case above: the caller first runs some app-specific command to
  deterministically make the right window frontmost (an `obsidian://`
  vault URI, a CLI tool, whatever the app supports), *then* calls `place
  --focused` to act on whatever that left focused - the frontmost-making
  and the placement are two separate steps, this only does the second.

Matching is strict either way: 0 matches or 2+ matches is an error, not a
guess - see "Design decisions" above for why.

```
lanes-wm apply
```
The primary interface: reads a JSON array of placement requests from stdin
and applies them as a single batch (one call, not a loop of single
placements) - this is how a lane's whole window layout gets applied in one
shot. Each `app` is resolved once, then every matched window for that app is
placed:
```json
[{"app": "com.github.wez.wezterm", "monitor": "37D8832A-...", "position": "full", "title": "main"}]
```
`title` and `focused` are optional, same semantics as `place`'s flags, and
still mutually exclusive - a request setting both is a per-item failure, not
a crash (JSON input bypasses the CLI's flag-conflict check, so it's
re-validated here). Writes one result per input to stdout, so a partial
failure doesn't abort the batch:
```json
[{"app": "com.github.wez.wezterm", "ok": true}]
```

```
lanes-wm request-access
```
Triggers the native macOS "lanes-wm would like to control this computer
using accessibility features" prompt (via `AXIsProcessTrustedWithOptions`),
or reports that access is already granted. Use this instead of manually
adding the binary through the Accessibility settings `+` button, which
doesn't reliably register raw (non-`.app`-bundle) executables.

## Local dev setup: Accessibility permissions

Placing windows requires the Accessibility (TCC) permission grant (System
Settings > Privacy & Security > Accessibility). Getting this to survive
repeated `cargo build` during development takes a couple of one-time setup
steps, because of two macOS quirks:

**1. Ad-hoc signing changes identity on every rebuild.** `codesign --sign -`
(ad-hoc signing) bakes a hash of the binary's own content into its identity,
so every rebuild looks like a brand new, never-approved program to macOS and
silently drops the grant. The fix is to sign with a real (self-signed,
local-only is fine) certificate instead, so the binary's Designated
Requirement is based on the certificate, not the content hash, and the grant
survives rebuilds. One-time setup:
1. Keychain Access > Certificate Assistant > Create a Certificate: name it
   (e.g. `lanes-wm dev`), type "Code Signing", Identity Type "Self Signed
   Root".
2. In Keychain Access, find that certificate, open it, expand "Trust", and
   set "Code Signing" to "Always Trust".
3. Run `./sign.sh` after every `cargo build` (or wire it into your build
   workflow) - it re-signs `target/debug/lanes-wm` with that certificate and
   a stable bundle identifier (`com.lanes.lanes-wm`).

**2. Raw executables need an embedded `Info.plist`.** TCC identifies apps by
bundle identifier, which for a normal `.app` comes from its bundle's
`Info.plist`. A raw Unix executable (no `.app` wrapper) has no such thing by
default, so `build.rs` embeds `Info.plist` directly into the binary as a
`__TEXT,__info_plist` Mach-O section (linker `-sectcreate`) at build time.
This happens automatically on every `cargo build` - no manual step needed.

With both in place: run `lanes-wm request-access` once to trigger the
permission prompt, click Allow, and it should stick across rebuilds as long
as you keep re-running `./sign.sh` after each build.

If you ever end up in a genuinely broken TCC state (grant shown as on in
Settings but `AXIsProcessTrusted()` still reports false, entries that won't
reset via `tccutil reset Accessibility <bundle-id>`), a full reboot has been
the only reliable fix found so far - the TCC daemon (`tccd`) appears to
cache stale state that `sudo killall tccd` doesn't fully clear.

**Do not run `tccutil reset Accessibility` with no bundle ID** - that resets
the grant for every app on the system, not just this one. Always pass the
bundle identifier.

## The long-term arc

Hotkey capture (the thing that triggers a lane switch at all) currently
lives in Hammerspoon, outside this repo. If that's ever rewritten in Rust
(CGEventTap-based, likely absorbed into lanes' `switch-ui` since it's
already a persistent Rust/Tauri process), this tool doesn't need to change -
it was designed to be invoked synchronously by *something*, and it doesn't
care what that something is. That's the reason for the CLI/JSON contract
over anything more tightly coupled to Hammerspoon or to lanes-cli itself.
