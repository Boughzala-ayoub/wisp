# Wisp

A Windows terminal built on [libghostty-vt](https://github.com/ghostty-org/ghostty), the terminal
emulation core of Ghostty. The goal is for PowerShell, Claude Code and Codex to feel the way they do
in Ghostty on a Mac: fast, sharp text, nothing in the way.

> Working name. [WispTerm](https://github.com/xuzhougeng/wispterm) is a different project, and this
> one gets renamed before any public release.

![Wisp running PowerShell](docs/devlog/m1-powershell.png)

## Status

Early. Milestone 1 of 7 is done: a window running PowerShell through ConPTY, drawn with Direct3D 11
and DirectWrite. See [PLAN.md](PLAN.md) for the milestones and [DEVLOG.md](DEVLOG.md) for progress
with measured numbers.

## How it works

Ghostty doesn't ship for Windows, and upstream recommends building Windows terminals on
libghostty-vt instead of porting the app. That library parses VT output and keeps the terminal
state. Wisp adds everything around it in Rust:

- `pty.rs` runs the shell inside a Windows pseudoconsole (ConPTY).
- `term.rs` feeds the shell's output to libghostty-vt and answers its queries.
- `input.rs` sends keys in conhost's win32-input-mode, so key combinations like Shift+Enter
  survive the trip through ConPTY.
- `font.rs` and `render.rs` rasterize glyphs with DirectWrite and draw each frame as one instanced
  Direct3D 11 call.
- `window.rs` owns the Win32 window and paces frames to the display with a waitable swap chain.

## Building

You need Windows 11, Rust (MSVC toolchain), the Visual Studio Build Tools and git.

```powershell
. .\scripts\bootstrap.ps1   # fetches Zig 0.15.2 and the pinned Ghostty source into .tools\
cargo run --release
cargo test
```

## Credits

Wisp exists because of [Ghostty](https://ghostty.org) by Mitchell Hashimoto and the Ghostty
contributors, and because they made its core reusable as libghostty. Wisp is an independent project.
It is not affiliated with or endorsed by the Ghostty project.

The Rust bindings come from [libghostty-rs](https://github.com/uzaaft/libghostty-rs). The ConPTY
lessons in [PLAN.md](PLAN.md) come from the Windows ports that went first: noctty, Wintty,
GhosttyWin32 and WispTerm.

Licensed under MIT. See [LICENSE](LICENSE) and [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
