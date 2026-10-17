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

## Rust crates

- libghostty-vt and libghostty-vt-sys, https://github.com/uzaaft/libghostty-rs, MIT OR Apache-2.0
- windows, https://github.com/microsoft/windows-rs, MIT OR Apache-2.0

The full dependency tree is in `Cargo.lock`. A complete notice file with every
license text gets generated before the first release.
