#!/bin/sh
# Re-sign the dev binary after each `cargo build`.
#
# Ad-hoc signing (`--sign -`) bakes in a hash of the binary's own content,
# so every rebuild looks like a brand new program to macOS and silently
# drops the Accessibility permission grant (System Settings > Privacy &
# Security > Accessibility). Signing with a real (self-signed, local-only)
# certificate gives it a stable identity - a Designated Requirement based
# on the certificate, not the content hash - so the grant survives rebuilds.
#
# One-time setup for this certificate is documented in README.md.
set -eu
codesign --force --sign "lanes-wm dev" --identifier com.lanes.lanes-wm target/debug/lanes-wm
