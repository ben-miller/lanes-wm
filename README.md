# lanes-wm

A standalone, lane-agnostic window-placement primitive for macOS. Given an
app and a monitor, move that app's window into a preset position on it.
Built to be called from [lanes](../lanes) on lane focus, but doesn't know
what a "lane" is.

## Status

Early scaffold. The CLI shape (`main.rs`) is defined; the actual macOS calls
are `todo!()`. Not yet wired into lanes-cli.

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

```
lanes-wm place --app <bundle-id> --monitor <uuid> --position <preset>
```
Single placement. Exit 0 on success; non-zero + stderr message on failure
(app not running, monitor not found, etc).

```
lanes-wm apply
```
Reads a JSON array of placement requests from stdin:
```json
[{"app": "com.github.wez.wezterm", "monitor": "37D8832A-...", "position": "full"}]
```
Writes one result per input to stdout, so a partial failure doesn't abort
the batch:
```json
[{"app": "com.github.wez.wezterm", "ok": true}]
```

## The long-term arc

Hotkey capture (the thing that triggers a lane switch at all) currently
lives in Hammerspoon, outside this repo. If that's ever rewritten in Rust
(CGEventTap-based, likely absorbed into lanes' `switch-ui` since it's
already a persistent Rust/Tauri process), this tool doesn't need to change -
it was designed to be invoked synchronously by *something*, and it doesn't
care what that something is. That's the reason for the CLI/JSON contract
over anything more tightly coupled to Hammerspoon or to lanes-cli itself.
