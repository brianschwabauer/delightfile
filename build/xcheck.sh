#!/usr/bin/env bash
# Type-check delightfile for macOS or Windows on a Linux machine.
#
#   build/xcheck.sh aarch64-apple-darwin check -p df-core --tests
#   build/xcheck.sh aarch64-apple-darwin clippy -p df-app --all-targets -- -D warnings
#   build/xcheck.sh x86_64-pc-windows-msvc clippy -p df-core -p df-app --all-targets -- -D warnings
#
# The first argument is the target; the rest is a cargo command, which gets
# `--target <target>` after its subcommand. Only `check` and `clippy` mean
# anything here: nothing this produces links, let alone runs. The runners
# build, link and test (plans/other-platforms/06-build-and-release.md); this is
# for finding a type error in a minute rather than in a twelve-minute CI run.
#
# Why a toolchain of its own. The machine's rust is Arch's `rust` package, which
# builds and tests Linux and has no standard library for any other target.
# Replacing it with rustup would change the compiler every Linux build uses, so
# the foreign targets live in a private rustup under ~/.cache/delightfile-xcheck
# (its own RUSTUP_HOME and CARGO_HOME), set for this script's cargo alone, at
# the system compiler's version. The script says how to make it if it is not
# there (00-ground-rules.md §6).
#
# Why stand-ins. df-core is plain Rust and needs nothing more. df-app has two
# build scripts that want the target's C toolchain: ffmpeg-sys-next runs
# bindgen over the FFmpeg headers, and libsqlite3-sys compiles its bundled
# SQLite. `check` needs their output to exist, not to be right, so:
#
# - FFMPEG_DIR names a directory whose `include` holds links to the host's
#   FFmpeg header folders (/usr/include/libav*, libsw*): Arch's FFmpeg 9 is
#   the API the runners have. The feature probe ffmpeg-sys-next compiles and
#   runs is built for the host, by the host's cc, as it is on every machine.
# - bindgen reads those headers as a clang target with the real target's C
#   data model, so the layout checks it writes agree with what `c_long` and
#   `size_t` are on the Rust target. For macOS that is x86_64 Linux: LP64
#   either way, with the host's C library under it. For Windows it is MSVC
#   itself, whose `long` is 32 bits, and since glibc's headers only know
#   LP64, bindgen is given a handful of C library headers written here that
#   declare what FFmpeg's headers use and no more; clang's own headers supply
#   the rest. Only the Rust types matter to `check`.
# - CC and AR for the target are two small scripts that write an empty object
#   and an empty archive wherever they are asked to.
#
# The stand-ins are written under ~/.cache/delightfile-xcheck/stand-ins on
# every run, and the build goes to ~/.cache/delightfile-xcheck/target-<target>,
# so the worktree's own target/ and the system toolchain are never touched.

set -euo pipefail

x="$HOME/.cache/delightfile-xcheck"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
    echo "usage: build/xcheck.sh <aarch64-apple-darwin|x86_64-pc-windows-msvc> <cargo subcommand> [args…]" >&2
    exit 2
}

target="${1:-}"
case "$target" in
    aarch64-apple-darwin | x86_64-pc-windows-msvc) ;;
    *) usage ;;
esac
shift
[ $# -gt 0 ] || usage

version="$(rustc -V 2>/dev/null | cut -d' ' -f2 || true)"
[ -n "$version" ] || version=stable

rustup_env="RUSTUP_HOME=$x/rustup CARGO_HOME=$x/cargo"
if [ ! -x "$x/cargo/bin/cargo" ] || [ ! -d "$x/rustup/toolchains" ]; then
    cat >&2 <<EOF
The cross-check toolchain is not at $x.
Make it once (it leaves the system's rust alone; see 00-ground-rules.md §6):

  mkdir -p $x
  curl --proto '=https' --tlsv1.2 -sSf -o $x/rustup-init \\
    https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init
  chmod +x $x/rustup-init
  $rustup_env \\
    $x/rustup-init -y --no-modify-path --profile minimal \\
    --default-toolchain $version --component clippy \\
    --target aarch64-apple-darwin --target x86_64-pc-windows-msvc
EOF
    exit 1
fi
if ! compgen -G "$x/rustup/toolchains/*/lib/rustlib/$target" >/dev/null; then
    cat >&2 <<EOF
The cross-check toolchain has no standard library for $target. Add it:

  $rustup_env $x/cargo/bin/rustup target add $target
EOF
    exit 1
fi
if [ ! -f /usr/include/libavcodec/avcodec.h ]; then
    echo "No FFmpeg headers under /usr/include (the ffmpeg package): df-app's bindings are made from them." >&2
    exit 1
fi

# ── The stand-ins ────────────────────────────────────────────────────────────

stand_ins="$x/stand-ins"
[ -L "$stand_ins/ffmpeg/include" ] && rm "$stand_ins/ffmpeg/include"
mkdir -p "$stand_ins/bin" "$stand_ins/ffmpeg/lib" "$stand_ins/ffmpeg/include" "$stand_ins/msvc-libc"
for dir in /usr/include/libav* /usr/include/libsw* /usr/include/libpostproc; do
    [ -d "$dir" ] && ln -sfn "$dir" "$stand_ins/ffmpeg/include/"
done

# The C library as FFmpeg's public headers use it, for clang reading them as
# MSVC: the types and functions their inline bodies name, at MSVC's sizes and
# with its errno numbers. stddef.h, stdint.h, stdarg.h and limits.h are
# clang's own.
cat >"$stand_ins/msvc-libc/stdio.h" <<'EOF'
#pragma once
#include <stddef.h>
#include <stdarg.h>
typedef struct _iobuf FILE;
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
int snprintf(char *buf, size_t size, const char *format, ...);
int vsnprintf(char *buf, size_t size, const char *format, va_list args);
EOF
cat >"$stand_ins/msvc-libc/stdlib.h" <<'EOF'
#pragma once
#include <stddef.h>
void abort(void);
void *malloc(size_t size);
void free(void *p);
EOF
cat >"$stand_ins/msvc-libc/string.h" <<'EOF'
#pragma once
#include <stddef.h>
int memcmp(const void *a, const void *b, size_t n);
void *memcpy(void *dst, const void *src, size_t n);
void *memset(void *dst, int c, size_t n);
size_t strlen(const char *s);
EOF
cat >"$stand_ins/msvc-libc/math.h" <<'EOF'
#pragma once
double fabs(double x);
double floor(double x);
double ceil(double x);
EOF
cat >"$stand_ins/msvc-libc/errno.h" <<'EOF'
#pragma once
#define EPERM 1
#define ENOENT 2
#define EIO 5
#define EAGAIN 11
#define ENOMEM 12
#define EACCES 13
#define EEXIST 17
#define EINVAL 22
#define ENOSPC 28
#define EPIPE 32
#define EDOM 33
#define ERANGE 34
#define ENOSYS 40
#define ENOTSUP 129
#define ETIMEDOUT 138
EOF
cat >"$stand_ins/msvc-libc/time.h" <<'EOF'
#pragma once
typedef long long time_t;
struct tm {
    int tm_sec, tm_min, tm_hour, tm_mday, tm_mon, tm_year, tm_wday, tm_yday, tm_isdst;
};
EOF
cat >"$stand_ins/msvc-libc/inttypes.h" <<'EOF'
#pragma once
#include <stdint.h>
#define PRId32 "d"
#define PRIu32 "u"
#define PRIx32 "x"
#define PRIX32 "X"
#define PRId64 "lld"
#define PRIu64 "llu"
#define PRIx64 "llx"
#define PRIX64 "llX"
EOF

# A C compiler that writes an empty object wherever it is told to: `-o path`
# as cc-rs passes it to a gcc-like compiler, `-Fo<path>` or `/Fo<path>` as it
# passes it to an MSVC-like one. Asked only to preprocess (cc-rs's probe of
# which compiler it has), it says nothing, and cc-rs takes it for gcc.
cat >"$stand_ins/bin/cc" <<'EOF'
#!/bin/sh
out=""
prev=""
for a in "$@"; do
    case "$a" in
        -Fo*) out="${a#-Fo}" ;;
        /Fo*) out="${a#/Fo}" ;;
    esac
    if [ "$prev" = "-o" ]; then out="$a"; fi
    prev="$a"
done
if [ -n "$out" ]; then : >"$out"; fi
exit 0
EOF

# An archiver that writes an empty archive: the first `*.a` or `*.lib` it is
# given (`ar cq lib.a …`), or lib.exe's `-out:`/`/OUT:` one.
cat >"$stand_ins/bin/ar" <<'EOF'
#!/bin/sh
for a in "$@"; do
    case "$a" in
        -out:* | -OUT:*) printf '!<arch>\n' >"${a#-*:}"; exit 0 ;;
        /out:* | /OUT:*) printf '!<arch>\n' >"${a#/*:}"; exit 0 ;;
        *.a | *.lib) printf '!<arch>\n' >"$a"; exit 0 ;;
    esac
done
exit 0
EOF
chmod +x "$stand_ins/bin/cc" "$stand_ins/bin/ar"

case "$target" in
    aarch64-apple-darwin) clang_args="--target=x86_64-unknown-linux-gnu" ;;
    x86_64-pc-windows-msvc) clang_args="--target=x86_64-pc-windows-msvc -isystem $stand_ins/msvc-libc" ;;
esac

var="${target//-/_}"
export "CC_$var=$stand_ins/bin/cc"
export "AR_$var=$stand_ins/bin/ar"
export "BINDGEN_EXTRA_CLANG_ARGS_$var=$clang_args"
export FFMPEG_DIR="$stand_ins/ffmpeg"

export RUSTUP_HOME="$x/rustup"
export CARGO_HOME="$x/cargo"
export CARGO_TARGET_DIR="$x/target-$target"
export PATH="$x/cargo/bin:$PATH"

cd "$repo"
subcommand="$1"
shift
exec cargo "$subcommand" --target "$target" "$@"
