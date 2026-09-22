# Vendored OpenTUI native renderer

- Upstream: <https://github.com/anomalyco/opentui>
- Release: `v0.5.9`
- Commit: `df2fc1594bb7a1274fc490155305e3d9f61f1b01`
- Zig: `0.16.0`

Haarness vendors the upstream `packages/native` tree, plus the core media test
fixtures referenced by the native suite, and embeds libraries built from that
source. This keeps normal Cargo builds offline and preserves the one-binary
distribution: Zig is needed only when regenerating the native archives.

## Local patch

Apple Terminal does not implement 24-bit SGR colors, even when the environment
contains `COLORTERM=truecolor`. OpenTUI v0.5.9 already contains a palette-aware
ANSI-256 renderer, so the Haarness patch only
adds the missing terminal capability override in `native/src/terminal.zig`.

The native tests prove the complete behavior: Apple Terminal is detected as
ANSI-256 even when `COLORTERM` claims truecolor, foreground colors use `38;5`
indexed sequences rather than unsupported `38;2` RGB, and a transparent page
background emits terminal-default `49` rather than a full-screen indexed fill.

## Regeneration

Run `./vendor/opentui/build-archives.sh /path/to/zig-0.16.0/zig` from the
repository root. The script builds all eight upstream targets and writes
deterministic archives under `vendor/opentui/artifacts`.
