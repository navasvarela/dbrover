#!/usr/bin/env bash
# Downloads DuckDB's own prebuilt library for one Rust target, checks it against the SHA-256
# pinned in SHA256SUMS beside this script, and prints the environment that makes libduckdb-sys
# link it instead of compiling DuckDB (see docs/duckdb-prebuilt.md):
#
#   packaging/duckdb/fetch.sh aarch64-apple-darwin target/duckdb >> "$GITHUB_ENV"
#
# `packaging/duckdb/fetch.sh --version` prints only the DuckDB version, which CI puts in the key
# of the Actions cache that keeps the downloads (.github/actions/duckdb/action.yml).
#
# The build must also pass `--cfg dbrover_prebuilt_duckdb` in RUSTFLAGS, which drops the
# `bundled` feature from dbrover-driver-duckdb's duckdb dependency.
#
# The DuckDB version is the one libduckdb-sys encodes in Cargo.lock (1.MMmmpp.x is DuckDB
# MM.mm.pp), so the library always matches the bindings libduckdb-sys ships.
set -euo pipefail

usage() {
    echo "usage: $0 <rust-target> <download-dir>" >&2
    echo "       $0 --version" >&2
    exit 2
}
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)

duckdb_version() {
    local encoded
    encoded=$(awk '/^name = "libduckdb-sys"$/ { getline; gsub(/"/, "", $3); print $3 }' \
        "$repo/Cargo.lock" | cut -d. -f2)
    [[ -n "$encoded" ]] || { echo "libduckdb-sys is missing from Cargo.lock" >&2; exit 1; }
    echo "$((10#$encoded / 10000)).$((10#$encoded / 100 % 100)).$((10#$encoded % 100))"
}

if [[ $# -eq 1 && "$1" == --version ]]; then
    duckdb_version
    exit
fi
[[ $# -eq 2 ]] || usage
target=$1
download_dir=$2

# Static libraries on Linux and macOS. DuckDB publishes no static library built with MSVC, so
# Windows links the DLL, which the installer ships beside dbrover.exe.
case "$target" in
    aarch64-apple-darwin) archive=static-libs-osx-arm64.zip kind=static ;;
    x86_64-apple-darwin) archive=static-libs-osx-amd64.zip kind=static ;;
    x86_64-unknown-linux-gnu) archive=static-libs-linux-amd64.zip kind=static ;;
    aarch64-unknown-linux-gnu) archive=static-libs-linux-arm64.zip kind=static ;;
    x86_64-pc-windows-msvc) archive=libduckdb-windows-amd64.zip kind=dynamic ;;
    *) echo "no prebuilt DuckDB for $target" >&2; exit 1 ;;
esac

version=$(duckdb_version)
pinned=$(awk -v name="v$version/$archive" '$2 == name { print $1 }' "$here/SHA256SUMS")
if [[ -z "$pinned" ]]; then
    echo "no checksum for v$version/$archive in $here/SHA256SUMS" >&2
    exit 1
fi

sha256() {
    if command -v sha256sum > /dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi \
        | cut -d' ' -f1
}

mkdir -p "$download_dir"
download_dir=$(cd "$download_dir" && pwd)
zip="$download_dir/v$version-$archive"
lib_dir="$download_dir/v$version-$target"

if [[ ! -f "$zip" ]] || [[ "$(sha256 "$zip")" != "$pinned" ]]; then
    # GitHub's release downloads answer 504 now and then, sometimes for longer than the 7 seconds
    # three retries used to wait (2026-11-05, run 42045110073). Without --retry-delay, curl
    # waits 1 second and doubles that before each retry, so eight retries span about 4 minutes.
    # --retry-all-errors also retries what curl takes for a permanent failure, such as a
    # connection reset mid-transfer. The cost: a wrong pin's 404 fails after 4 minutes, not at once.
    curl -fsSL --retry 8 --retry-all-errors -o "$zip.part" \
        "https://github.com/duckdb/duckdb/releases/download/v$version/$archive"
    actual=$(sha256 "$zip.part")
    if [[ "$actual" != "$pinned" ]]; then
        rm -f "$zip.part"
        echo "checksum mismatch for $archive: expected $pinned, got $actual" >&2
        exit 1
    fi
    mv "$zip.part" "$zip"
    rm -rf "$lib_dir"
fi

if [[ ! -d "$lib_dir" ]]; then
    unzip -q -o "$zip" -d "$lib_dir.part"
    if [[ "$kind" == static ]]; then
        # DuckDB ships its core, each extension and each third-party library as archives of
        # their own, with references running both ways between them (the core calls the
        # generated extension loader, which calls the extensions, which call the core).
        # libduckdb-sys links one archive, duckdb_static; GNU ld resolves references within
        # one archive in any order but scans separate archives once each, so everything is
        # merged into that one. ld64 on macOS doesn't care either way.
        (
            cd "$lib_dir.part"
            mv libduckdb_static.a core.a
            parts=(core.a lib*.a)
            if [[ "$target" == *apple-darwin ]]; then
                libtool -static -o libduckdb_static.a "${parts[@]}"
            else
                { echo "create libduckdb_static.a"
                  printf 'addlib %s\n' "${parts[@]}"
                  echo save
                  echo end; } | ar -M
            fi
            rm -- "${parts[@]}"
        )
    fi
    mv "$lib_dir.part" "$lib_dir"
fi

# Git Bash on Windows: cargo wants C:\... rather than /c/...
if command -v cygpath > /dev/null; then
    lib_dir=$(cygpath -w "$lib_dir")
fi
echo "DUCKDB_LIB_DIR=$lib_dir"
# libduckdb-sys asks pkg-config first, which would find a DuckDB installed on the machine (by
# Homebrew or a distribution) and link that one instead.
echo "DUCKDB_NO_PKG_CONFIG=1"
if [[ "$kind" == static ]]; then
    echo "DUCKDB_STATIC=1"
fi
