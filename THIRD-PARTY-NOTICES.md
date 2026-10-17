# Third-party notices

Wisp's binary statically links the code below. Each project keeps its own license.

## Ghostty (libghostty-vt)

https://github.com/ghostty-org/ghostty, MIT License.

```
Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

libghostty-vt in turn bundles:

- simdutf, https://github.com/simdutf/simdutf, MIT OR Apache-2.0
- Highway, https://github.com/google/highway, Apache-2.0 OR BSD-3-Clause

## Color themes

The files in `themes/` come from
[iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes), release
`release-20261012-151317-d3e95e4`, the same collection Ghostty bundles. The collection is MIT
licensed, and the copyright of each theme belongs to its author. Authorship is listed in
[docs/themes-credits.md](docs/themes-credits.md), copied from the collection's `CREDITS.md`.

```
Copyright (c) 2011 to Present Mark Badolato

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## ConPTY (conpty.dll and OpenConsole.exe)

From Microsoft's [`Microsoft.Windows.Console.ConPTY`](https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY)
package, version 1.25.260930003, built from [microsoft/terminal](https://github.com/microsoft/terminal).
`scripts/bootstrap.ps1` downloads it and checks its SHA-512; Wisp runs without it.

```
Copyright (c) Microsoft Corporation. All rights reserved.

MIT License

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Rust crates

These crates are compiled into `wisp.exe`. Each is offered under MIT OR Apache-2.0, and Wisp uses
them under the MIT License, whose text is the same as Ghostty's above with the copyright lines below.

| Crate | Version | Copyright |
|---|---|---|
| [libghostty-vt, libghostty-vt-sys](https://github.com/uzaaft/libghostty-rs) | 0.2.2 | Copyright (c) 2026 Uzair Aftab, Leah Amelia Chen |
| [windows, windows-core, windows-link, windows-result, windows-strings, windows-numerics, windows-collections, windows-future, windows-threading](https://github.com/microsoft/windows-rs) | 0.62 | Copyright (c) Microsoft Corporation. |
| [bitflags](https://github.com/bitflags/bitflags) | 2.13.2 | Copyright (c) 2014 The Rust Project Developers |

Build-time only, not part of the binary: `int-enum`, `syn`, `quote`, `proc-macro2`,
`proc-macro2-diagnostics`, `unicode-ident`, `version_check`, `windows-implement` and
`windows-interface`. Exact versions are in `Cargo.lock`.
