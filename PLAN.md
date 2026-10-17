# Wisp plan

Research done 2026-10-17. No app code yet. This file records what I found, what I decided and why, and the milestone order.

## What I found

### Upstream Ghostty will not ship Windows

The Ghostty team closed the door this year. In [discussion #12371](https://github.com/ghostty-org/ghostty/discussions/12371) (2026-04-22) pluiedev wrote that "the Ghostty team does not plan to make an official Windows build anymore" and left Windows terminals to the community. On 2026-10-10 mitchellh [added](https://github.com/ghostty-org/ghostty/pull/14608#issuecomment-6101144062): "libghostty-vt and custom Windows builds around that are the proper way forward. We test libghostty-vt against Windows already." The old Windows issue #2563 is locked.

The discussion also sets a trademark rule. "Ghostty" must not appear in a third-party project's name or branding. winghostty had to rename to noctty because of it.

### libghostty comes in two pieces, and only one works on Windows

**libghostty-vt** (`include/ghostty/vt.h` plus 30 headers) is the part upstream supports on Windows. It contains the VT parser, terminal state with scrollback and reflow, a render-state API with per-row dirty flags for writing your own renderer, key and mouse encoders (Kitty keyboard protocol included), selection (word, line, all, formatted copy), paste safety checks, and OSC 52 clipboard callbacks. It starts no threads and has no renderer, PTY, font code or config parser. CI builds it on a Windows runner and links it from MSVC through CMake. There is no tagged release yet, and the header calls itself "work-in-progress... definitely going to change", so we pin a commit.

**Full libghostty** (`include/ghostty.h`, the API the macOS app embeds) has only macOS and iOS platforms, only OpenGL and Metal renderers, and the core stopped compiling for Windows after #14052. Using it means maintaining a Ghostty fork with a new platform and a new renderer.

### What the community ports learned

| Project | Approach | Renderer | Notes |
|---|---|---|---|
| [noctty](https://github.com/amanthanvi/noctty) | Zig fork of the whole repo, Win32 app runtime | OpenGL 4.3 (D3D11 beta) | Most complete. GDI chrome, no Mica. 441 stars, one maintainer |
| [Wintty](https://github.com/deblasis/wintty) | Fork + full embedding API, C# WinUI 3 | New DirectX 12 backend | Mica, vertical tabs. 190 open issues, several UI-thread stalls |
| [GhosttyWin32](https://github.com/i999rri/GhosttyWin32) | Fork + C++/WinRT WinUI 3 | New DirectX backend | Branch is 487 commits behind upstream |
| [shiweis/ghostty-windows](https://github.com/shiweis/ghostty-windows) | Zig fork, Win32 | OpenGL 4.6 | Has win32-input-mode and 463 themes |
| [WispTerm](https://github.com/xuzhougeng/wispterm) | libghostty-vt only, Zig | Own D3D11 renderer | 450 stars, MIT. Same architecture I'm proposing |

The lessons I'm taking from them:

1. Every fork had to write a Direct3D renderer, because upstream has none. A fork buys Ghostty's font stack and config parser, then charges for it in rebase work against a core that upstream no longer compiles on Windows.
2. The ConPTY that ships inside Windows (conhost) rewrites output, repaints on resize and drops mouse-mode passthrough. noctty, WispTerm and phalladar's port all bundle `conpty.dll` + `OpenConsole.exe` from Microsoft's MIT-licensed `Microsoft.Windows.Console.ConPTY` NuGet package and fall back to the in-box one.
3. ConPTY 1.22 and later sends a DA1 query on startup and holds output for up to 3 seconds until the terminal answers. Wintty's WSL panes stayed blank until they fixed this (#1268). libghostty-vt answers DA1 through its `WRITE_PTY` and `DEVICE_ATTRIBUTES` callbacks, so we register both from day one.
4. `ClosePseudoConsole` deadlocks if nobody keeps draining the output pipe. Close on a background thread while the reader keeps reading.
5. WinUI 3 ports pay in UI stalls. Wintty's first split blocks for about 400 ms (#1339) and layout switches stall 200 to 300 ms (#890). noctty measured 156 ms to first swap with a DirectComposition chrome and went back to GDI for it. Startup cost is something to measure, not assume.

### Rust bindings exist and build here

[libghostty-rs](https://github.com/uzaaft/libghostty-rs) (crates `libghostty-vt` 0.2.2 and `libghostty-vt-sys`, MIT OR Apache-2.0) wraps the whole vt API and its build script handles the Windows static library. It ships ghostling-rs, a 1,400-line example terminal. I built a probe against it on this machine. It compiled libghostty-vt with Zig 0.15.2, linked under MSVC, and parsed `hello \e[31mwisp\e[0m\r\nline2` to the expected cursor position.

One Windows catch came up. By default the build script clones Ghostty deep inside `target/` and git fails with "Filename too long". Pointing `GHOSTTY_SOURCE_DIR` at a pinned checkout on a short path fixes it. The crate pins Ghostty `a887df4` (2026-07-11), which needs Zig 0.15.2. Upstream main now needs Zig 0.16, so moving to a newer Ghostty means bumping Zig and the crate together.

All binding types are `!Send + !Sync`. The terminal lives on one thread.

### Name clash

WispTerm (wispterm.com) is an MIT terminal built on libghostty-vt with a D3D11 renderer. "Wisp" is close enough to cause confusion. That doesn't matter for a local project, but pick a different name before publishing anything.

## Decision

**Build on libghostty-vt and write the renderer, PTY layer and UI ourselves.** This is the path upstream recommends and tests, and it is how WispTerm got to a working D3D11 terminal without a fork. The cost is that we write font rendering and a config parser. The config format is small, and native DirectWrite text is what I'd want on Windows anyway.

### Language: Rust

- The libghostty-vt bindings are maintained, Windows-aware, and verified on this machine.
- The `windows` crate (Microsoft's official bindings, 0.62) covers Win32, D3D11, DXGI, DirectComposition, DirectWrite, Direct2D, DWM and ConPTY.
- Rust 1.97 + MSVC are already installed, and the output is one static exe with no runtime.

Zig would match Ghostty, but its Win32 bindings are thin and the 0.15 to 0.16 churn is already visible in the ports. C# and WinUI bring GC and startup costs (Wintty's callback-pinning crashes, pain point P5 in [wintty#98](https://github.com/deblasis/wintty/issues/98)).

### UI framework: plain Win32

One top-level `HWND` per window, no XAML. The tab bar sits in the title bar area (`DwmExtendFrameIntoClientArea` + `WM_NCCALCSIZE` + `WM_NCHITTEST`, the same trick Windows Terminal uses), and our own renderer draws it. Splits are viewports inside the same swap chain, so a split costs a layout pass and no new device. This avoids the WinUI stalls above and keeps startup fast.

### Renderer: Direct3D 11 + DirectWrite

- D3D11 is enough for a terminal, and Windows Terminal's AtlasEngine proves it. D3D12 adds complexity without a visible win at this scale. WARP is the fallback for VMs and broken drivers.
- A flip-model swap chain created for composition (`CreateSwapChainForComposition`, premultiplied alpha) and attached through DirectComposition, so Mica or acrylic shows through the background. With opacity at 1.0 we can use a plain HWND swap chain and skip DComp if startup measures slow.
- Glyphs. DirectWrite shapes each run of same-styled cells with `IDWriteTextAnalyzer` (`calt` and `liga` on, which covers ligatures in Cascadia Code, JetBrains Mono and Fira Code). Direct2D rasterizes glyphs into a grayscale atlas texture, with color glyphs for emoji. `IDWriteFontFallback` covers emoji, CJK and Nerd Font symbols. This is the Windows equivalent of Ghostty using CoreText on macOS.
- Each frame is two instanced draws (cell backgrounds, then glyphs) plus cursor and selection quads. We redraw only when something changed, using a waitable swap chain with max frame latency 1, so a keystroke echo goes out on the next vblank.

### PTY: ConPTY, keyboard via win32-input-mode

- `CreatePseudoConsole`, with `dwFlags = 0` (Wintty found flags that deadlock on close). Load a bundled `conpty.dll` next to the exe if present, otherwise use kernel32's.
- Every Windows console app sits behind conhost, so keys are best delivered the way Windows Terminal delivers them. When ConPTY asks for win32-input-mode (`ESC[?9001h`), we send `ESC[Vk;Sc;Uc;Kd;Cs;Rc_` sequences and conhost builds exact key events for the app. That is what makes Shift+Enter, Ctrl+Space and similar keys reach PowerShell and Claude Code intact. libghostty-vt's key encoder is the fallback when that mode is off. I'll confirm in M1 by logging what ConPTY sends at startup.
- Threads. One reader thread per pane does blocking `ReadFile` and posts chunks to the UI thread, which owns the terminals, feeds `vt_write`, and renders. libghostty-vt's parser is fast enough that I expect this to hold. If throughput tests show input lag during `cat bigfile`, move each terminal to its own thread behind a lock (the render state's `begin_update`/`end_update` exists to keep that lock short).

### Config and themes

- Ghostty syntax (`key = value`, `#` comments only at line start, repeatable keys append, `key =` resets, `config-file = ?optional/path` includes). Our own parser, around 200 lines. Unknown keys log a warning and are skipped, so a Mac Ghostty config loads without errors.
- Path is `%XDG_CONFIG_HOME%\wisp\config.ghostty` if `XDG_CONFIG_HOME` is set, else `%LOCALAPPDATA%\wisp\config.ghostty`. This mirrors where Ghostty's never-shipped Windows code would look (`src/os/xdg.zig`).
- Themes use Ghostty's theme format and the same `theme = name`, `theme = light:X,dark:Y` syntax. We bundle the `ghostty/` folder of [iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes) (MIT collection, per-theme authorship kept in its CREDITS), and user themes in `%LOCALAPPDATA%\wisp\themes` take priority.
- MVP keys are `font-family`, `font-size`, `font-feature`, `theme`, `background`, `foreground`, `palette`, `cursor-color`, `cursor-style`, `selection-background`, `selection-foreground`, `background-opacity`, `background-blur`, `window-padding-x/y`, `command`, `scrollback-limit`, `keybind`, `copy-on-select`, `right-click-action`, `confirm-close-surface` and `config-file`. Wisp-only settings get a `wisp-` prefix (for example `wisp-backdrop = mica | acrylic | none`) so they can't collide with future Ghostty keys.

### Defaults

- Font is Cascadia Code 12pt. It ships with Windows 11 and has ligatures. Consolas is the fallback.
- Keybinds follow Ghostty's Linux defaults, read from `src/config/Config.zig`: `ctrl+shift+c/v` copy and paste, `ctrl+insert`/`shift+insert`, `ctrl+shift+t` new tab, `ctrl+shift+w` close, `ctrl+page_up/page_down` and `ctrl+shift+left/right` switch tabs, `ctrl+shift+o` split right, `ctrl+shift+e` split down, `ctrl+alt+arrows` move between splits, `ctrl+=`/`ctrl+-`/`ctrl+0` font size, `ctrl+shift+,` reload config.
- Right-click is `copy-or-paste` (a real Ghostty value): copy if there is a selection, paste otherwise. Windows users expect that from conhost and Windows Terminal. Ghostty's own default is `context-menu`.
- Shell is `pwsh.exe` if installed, else Windows PowerShell. The new-tab menu lists cmd, Git Bash and each WSL distro it finds.

## Layout

```
wisp/
  Cargo.toml
  .cargo/config.toml     # GHOSTTY_SOURCE_DIR -> .tools/ghostty (short path)
  .tools/                # gitignored: zig 0.15.2, ghostty checkout @ a887df4
  src/
    main.rs              # entry, message loop
    window.rs            # HWND, titlebar, tabs, splits, DPI, backdrop
    pty.rs               # ConPTY spawn/read/write/resize/close
    term.rs              # one pane: libghostty-vt terminal + pty + callbacks
    input.rs             # VK -> win32-input-mode / libghostty key events
    render.rs            # D3D11 swap chain, atlas, draw
    font.rs              # DirectWrite shaping, fallback, rasterization
    config.rs            # Ghostty config parser + themes
  themes/                # bundled from iTerm2-Color-Schemes
  tests/                 # integration tests (ConPTY, render readback)
```

Files get split when they get too big, not before.

## Milestones

Each milestone ends with `cargo test` green, a release build that I launch and screenshot myself, and a local commit.

1. **Window + PowerShell.** Win32 window, D3D11 grid renderer (DirectWrite atlas, no shaping yet), ConPTY running PowerShell, keyboard input, resize, per-monitor DPI v2, DA1 and other replies through libghostty-vt callbacks. Tests: ConPTY round trip (`cmd /c echo` into a libghostty terminal, assert screen text), resize, and an offscreen WARP render with pixel readback.
2. **Text quality.** Shaping and ligatures, font fallback (emoji, CJK, Nerd Font glyphs), bold, italic, underline styles, wide characters, cursor styles and blink. Tests: run segmentation and cluster-to-cell mapping, plus a rendered ligature checked by readback.
3. **Mouse and clipboard.** Drag, double- and triple-click selection, right-click copy-or-paste, `ctrl+shift+c/v`, bracketed paste with the unsafe-paste check, mouse reporting for TUIs, wheel scrollback, OSC 52. Tests: selection to clipboard text, paste encoding.
4. **Config and themes.** Parser, includes, themes, keybinds, reload on `ctrl+shift+,`. Tests: parser cases taken from Ghostty's docs, a real Mac config loads with only warnings, theme lookup order.
5. **Window chrome.** Mica, acrylic and opacity, custom title bar, tabs, splits (resize by dragging the divider, move focus). Tests: split tree layout math, tab add/close/reorder.
6. **Shells.** Shell detection (pwsh, PowerShell, cmd, Git Bash, WSL), new-tab menu, bundled OpenConsole with in-box fallback, IME commit text. Tests: profile detection, each shell spawns and prints a known string.
7. **Latency and polish.** Measure keypress-to-present time inside the app and `cat` throughput on a 100 MB file against Windows Terminal, check startup time, then write the README with credits and third-party notices.

IME preedit (showing text while composing) and Kitty graphics come after MVP.

## Risks

- **libghostty-vt API churn.** It's pre-1.0. We pin one commit and update deliberately.
- **In-box ConPTY quirks.** Mouse passthrough, resize repaint and DA1 delays. Bundling OpenConsole in M6 is the fix, and M1 to M5 run on the in-box one.
- **win32-input-mode details.** If conhost on this build doesn't request it, we fall back to libghostty-vt's key encoder and lose some key combos inside console apps.
- **Glyph quality.** Getting text to look as crisp as Windows Terminal is fiddly (gamma, contrast, subpixel positioning). Grayscale AA first, ClearType later if it's worth it.
- **Mica plus transparency.** Needs the DirectComposition path, and noctty measured its startup cost at 156 ms. I'll measure ours.

## Licensing and credits

- Ghostty and libghostty-vt are MIT. The README credits Ghostty, says Wisp is not affiliated with or endorsed by the Ghostty project, and keeps "Ghostty" out of Wisp's name and logo.
- libghostty-rs is MIT OR Apache-2.0. The `windows` crate is MIT OR Apache-2.0.
- Bundled themes from iTerm2-Color-Schemes keep its LICENSE and CREDITS.
- Bundled `conpty.dll`/`OpenConsole.exe` are MIT (microsoft/terminal).
- A `THIRD-PARTY-NOTICES.md` lists every dependency we link or ship. I'm borrowing ideas from the ports above, not code. If that changes, the copied file keeps its copyright line and gets a notice entry.

## Open questions

1. Keep the name "Wisp" given WispTerm, or rename now?
2. Windows 11 only, or should Windows 10 work too (no Mica, solid background)?
3. Cascadia Code as the default font, or bundle JetBrains Mono (OFL) the way Ghostty does?
