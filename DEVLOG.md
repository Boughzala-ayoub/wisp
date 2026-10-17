# Wisp devlog

Notes per milestone, with numbers measured on one machine: a laptop with an RTX 3050 and Intel UHD
graphics, Windows 11 build 26300, one 1920x1080 display at 100% scaling.

## M1: a window that runs PowerShell

*2026-10-17*

![PowerShell in Wisp, showing colors, bold, italic, underline and strikethrough](docs/devlog/m1-powershell.png)

Wisp opens a window, starts PowerShell in a pseudoconsole, feeds its output through libghostty-vt
and draws the screen with Direct3D 11. Typing works, resizing reflows, and closing the shell closes
the window. Bold and italic use the font's real faces, and the title bar takes the terminal's
background color so the window reads as one surface.

| | |
|---|---|
| Launch to first frame | 122 to 144 ms over six warm launches, measured from process creation |
| Binary | 1.9 MB, one exe, no runtime |
| Tests | 11, including a real ConPTY round trip and an offscreen render checked pixel by pixel |

Input latency and throughput get measured properly in M7. Startup also gets a closer look there. I
haven't profiled it yet, and my guess is that most of the 130 ms is Direct3D device creation on a
hybrid-GPU laptop plus compiling shaders at runtime.

Things that surprised me:

- **The first bytes ConPTY sends are a request.** `ESC[?9001h ESC[?1004h` asks the terminal to
  send keys in "win32-input-mode" and to report focus changes. In that mode every key event travels
  as `ESC[Vk;Sc;Uc;Kd;Cs;Rc_`, the raw Windows key record, and conhost rebuilds it for the program.
  That is how Shift+Enter can reach a console program at all. A test types `echo w32ok` into cmd
  this way and checks the screen.
- **Redirected stdio leaks past the pseudoconsole.** Under `cargo test`, `cmd /c echo` printed to
  the test runner's console instead of into ConPTY. When the parent's stdio is redirected, the child
  inherits those handles unless `STARTF_USESTDHANDLES` is set with null handles.
- **Windows path limits bite the build.** libghostty-vt-sys clones Ghostty deep inside `target/`
  and git fails with "Filename too long". A pinned checkout in `.tools/` fixes it.
- **Testing a GUI without stealing focus.** My first screenshot script typed with SendKeys, which
  goes to whatever window has focus. When Wisp didn't have it, the keystrokes went elsewhere. The
  script now posts messages straight to Wisp's window and captures with `PrintWindow`, so it works
  with the window in the background.

## M2: text that looks right

*2026-10-17*

![Ligatures, emoji, CJK, underline styles and box drawing in Wisp](docs/devlog/m2-text.png)

Cascadia Code's ligatures now form (`=>` becomes an arrow, `!=` becomes `≠`), emoji render in color,
and Chinese, Japanese and Korean text comes from the fonts Windows falls back to. All five underline
styles work, including colored curly underlines. The cursor blinks every 600 ms like Ghostty's and
turns hollow when the window loses focus.

Each row is split into runs of same-style text, and DirectWrite shapes each run. A run also ends at
the cursor, the same as Ghostty's default, so a ligature never forms around the character you're
typing. Characters the font lacks go to the system font fallback. Color emoji come from Segoe UI
Emoji's layered COLR glyphs, which Wisp composites into a second RGBA atlas.

| | |
|---|---|
| Building a full 120x40 frame (CPU) | 2.73 ms before caching shaped runs, 0.41 ms after |
| Launch to first frame | 127 to 189 ms over six warm launches, median about 144 ms |
| Tests | 25, plus an opt-in frame benchmark |

Things that surprised me:

- **Fonts can't draw seamless boxes.** Once a 9.6 px advance rounds to a 10 px cell, `─` leaves a
  gap on every cell and borders look dashed. Claude Code draws its whole UI with `╭─╮│╰─╯`, so
  that was not acceptable. Wisp now draws box drawing, block elements and Powerline arrows itself,
  sized to the exact cell, the way Ghostty does. The rounded corners are quarter circles placed so
  they meet the straight lines on both sides.
- **conhost keeps curly underlines.** ConPTY re-renders the console's buffer instead of passing
  output straight through, so I expected styles conhost doesn't model to disappear. Curly, dotted,
  dashed and colored underlines all survive the round trip on this Windows build.
- **libghostty-vt's cursor doesn't blink by default.** Ghostty's app blinks, so an embedder that
  wants the same behavior has to set the default blink state on the terminal itself.
- **Shaping dominates the frame.** Before the cache, almost all of the 2.7 ms went to DirectWrite
  analyzing and shaping runs that hadn't changed since the last frame.
