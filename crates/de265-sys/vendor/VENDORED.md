# Vendored libde265

- Version: **1.1.3** (tag `v1.1.3`, 2026-09-14), from https://github.com/strukturag/libde265
- License: LGPL-3.0-or-later for the library (see `COPYING`, which also holds the GPL-3.0 text
  it refers to). Peeroxide itself is Apache-2.0; the release zip carries this notice in
  `THIRD-PARTY-NOTICES.txt`.
- Copied unmodified: `libde265/*.cc`, `libde265/*.h`, `libde265/x86/*`, `extra/win32cond.*`,
  `COPYING`, `AUTHORS`.
- Added: `libde265/de265-version.h`, which CMake would generate from `de265-version.h.in`.
- Left out: the CMake files, the sample applications, the ARM32 assembly, and the AVX2/AVX-512
  kernels (libde265 only enables those with GCC/Clang; the SSE4.1 ones are used everywhere on
  x86-64).

`build.rs` compiles it with the `cc` crate and writes the `config.h` CMake would.

To update: replace these files from the new release, update the version here and in
`de265-version.h`, and check `SOURCES` in `build.rs` against `libde265/CMakeLists.txt`.
