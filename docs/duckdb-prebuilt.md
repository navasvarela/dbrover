# DuckDB's prebuilt libraries

Status: **adopted on 2026-10-25**, after a spike that measured both builds on all three
runners (the numbers below). CI and the release workflow link the libraries DuckDB publishes
with each release instead of compiling DuckDB's C++ engine (the `duckdb` crate's `bundled`
feature). A plain `cargo build` still compiles it, so developers need nothing new.

**Why.** Compiling DuckDB took 10–14 minutes per OS on CI's runners, twice in a cold CI run
and twice in every macOS release (once per architecture). Linking DuckDB's libraries roughly
halves a release build and cuts a cold CI run to between a quarter and a half. It also shrinks
CI's caches enough to fit under GitHub's 10 GB limit, which the compiled build's caches nearly
filled on their own. The cost: packages 2 to 6 MB larger (9 to 11%), mostly ICU and
autocomplete, which DuckDB's libraries carry.

## What DbRover links

| Platform | DuckDB artefact (v1.5.6 release) | How it links |
| --- | --- | --- |
| macOS arm64 | `static-libs-osx-arm64.zip` | static, into `dbrover` |
| macOS x86_64 | `static-libs-osx-amd64.zip` | static, into `dbrover`, then `lipo` as today |
| Linux x86_64 | `static-libs-linux-amd64.zip` | static, into `dbrover` |
| Windows x86_64 | `libduckdb-windows-amd64.zip` | `duckdb.dll`, shipped beside `dbrover.exe` |

DuckDB publishes no static library built with MSVC (only `static-libs-windows-mingw.zip`), and
DbRover builds with MSVC, so Windows links the DLL.

### How the build switches

`libduckdb-sys` (the crate under `duckdb`) has two build paths, chosen by its `bundled`
feature (see its `build.rs`):

- **`bundled`** compiles DuckDB's amalgamated sources with `cc`, with `core_functions`, plus
  `parquet` and `json` when those features are on. It ignores every `DUCKDB_*` variable.
- **Without `bundled`**, it links an existing library. With `DUCKDB_LIB_DIR` set it emits
  `rustc-link-search=<dir>` and `rustc-link-lib=dylib=duckdb`, or `static=duckdb_static` when
  `DUCKDB_STATIC` is set. It uses its own pregenerated bindings (no `bindgen`), so the library
  must be the DuckDB release the crate version encodes (1.10506.x is DuckDB 1.5.6).
  `DUCKDB_DOWNLOAD_LIB` would download the *dynamic* library, unchecked, so DbRover doesn't
  use it.

A Cargo feature can only turn `bundled` on, never off, so the switch is a `cfg`:

```toml
# crates/dbrover-driver-duckdb/Cargo.toml
[target.'cfg(not(dbrover_prebuilt_duckdb))'.dependencies]
duckdb = { workspace = true, features = ["bundled", "parquet", "json"] }

[target.'cfg(dbrover_prebuilt_duckdb)'.dependencies]
duckdb.workspace = true
```

Cargo evaluates `cfg(...)` in target tables with the flags in `RUSTFLAGS`, so
`RUSTFLAGS="--cfg dbrover_prebuilt_duckdb"` drops `bundled` and nothing else changes.
`cargo tree -e features -i libduckdb-sys` confirms it: `bundled`, `parquet` and `json` without
the flag, neither with it. Without the flag (every local build, docs.rs, rust-analyzer),
DuckDB is compiled as before. `Cargo.lock` doesn't change.

`packaging/duckdb/fetch.sh <rust-target> <dir>` does the rest:

1. Reads the DuckDB version from `libduckdb-sys` in `Cargo.lock` (1.10506.1 → 1.5.6).
2. Downloads the archive for the target from
   `github.com/duckdb/duckdb/releases/download/v1.5.6/`, and fails unless its SHA-256 matches
   the line for `v1.5.6/<archive>` in `packaging/duckdb/SHA256SUMS`. With no pin for the
   version or target, it fails too.
3. Static archives: merges DuckDB's 22 archives into one `libduckdb_static.a` (see
   [Linking order](#linking-order)).
4. Prints `DUCKDB_LIB_DIR=…`, `DUCKDB_NO_PKG_CONFIG=1` and, for static libraries,
   `DUCKDB_STATIC=1`, ready for `>> "$GITHUB_ENV"`.

`DUCKDB_NO_PKG_CONFIG=1` matters. `duckdb` enables `libduckdb-sys`'s default features, which
include `pkg-config`. Even with `DUCKDB_LIB_DIR` set, the crate asks pkg-config first, which
would find a DuckDB installed by Homebrew or a distribution and link that one instead.

`crates/dbrover-driver-duckdb/build.rs` adds the one link the static path misses: the C++
standard library (`c++` on macOS, `stdc++` on Linux), which `cc` adds in the bundled build.

The pins were checked against the SHA-256 digests GitHub records for each release asset
(`gh release view v1.5.6 -R duckdb/duckdb --json assets`).

## Findings

### 1. Static libraries on Linux and macOS

**Parquet and JSON are included and work.** Each static archive set has the core
(`libduckdb_static.a`, 61 MB on arm64, 82 MB on Linux), one archive per extension (`parquet`,
`json`, `core_functions`, `icu`, `autocomplete`, `tpch`, `tpcds`), DuckDB's third-party
libraries (`re2`, `fmt`, `zstd`, `miniz`, `mbedtls`, `yyjson`, `utf8proc`, `pg_query`,
`fastpforlib`, `fsst`, `hyperloglog`, `skiplistlib`, and `jemalloc` on Linux), and
`libduckdb_generated_extension_loader.a`. The loader is what loads extensions at startup. It
references `core_functions`, `parquet`, `json`, `icu` and `autocomplete`, so all five end up in
the binary. `tpch` and `tpcds` are never referenced, so the linker leaves them out.

The test `links_duckdb_1_5_6_with_parquet_and_json` (in `dbrover-driver-duckdb`) asserts
three things on every platform, bundled or prebuilt:

- `pragma_version()` is `v1.5.6`;
- `json` and `parquet` have `install_mode = 'STATICALLY_LINKED'`;
- `json_extract` answers.

The existing `queries_a_parquet_file_from_memory` writes and reads Parquet. Locally on macOS,
the statically linked extensions are `autocomplete, core_functions, icu, json, parquet`.

<a id="linking-order"></a>
**What to link, and in what order.** References run in a cycle: the core calls the loader,
the loader calls the extensions, and the extensions call the core. `libduckdb-sys` emits
exactly one library, `duckdb_static`. GNU ld scans each archive once, in command-line order,
but keeps rescanning a *single* archive until nothing new resolves. So `fetch.sh` merges all
22 archives into one `libduckdb_static.a` (`libtool -static` on macOS, an `ar -M` script on
Linux), and the order question disappears. ld64 on macOS resolves in any order anyway.

The only other library is the C++ runtime, added by the driver's `build.rs`. rustc puts native
dylibs after all rlibs, so `stdc++` lands after the archive that needs it, even with
`--as-needed`. Nothing else is needed: the Linux objects ask only for glibc
(`libm`, `libc`, `ld-linux`), plus `libgcc_s`, which Rust's std already links.

**Universal macOS binary: yes.** Each target is built on its own with its own
`DUCKDB_LIB_DIR`. The release job sets it per loop iteration, then `lipo` joins the two
binaries as today. The runner is arm64, so the spike also ran the x86_64 slice under Rosetta:
`dbrover --version` worked, and all 13 `dbrover-driver-duckdb` tests passed against the x86_64
static library.

### 2. Windows

`libduckdb-sys` links `duckdb.lib`, the import library from the zip, so `dbrover.exe` loads
`duckdb.dll` when it starts.

**Where it's found.** Windows looks in the executable's own folder first:

- **CI:** the job adds `DUCKDB_LIB_DIR` to `PATH`, so test binaries under `target/` find it.
- **Release:** the job copies the DLL into `target/release/` before the existing smoke test.
- **NSIS installer:** cargo-packager's `resources` apply to every platform, and there is no
  per-platform list. So the Windows job writes a config of its own: the
  `[package.metadata.packager]` table from `cargo metadata`, plus
  `{src: duckdb.dll, target: "duckdb.dll"}`. It saves this as
  `crates/dbrover-app/packager-windows.json`, beside `Cargo.toml`, so the relative paths in
  `Cargo.toml` still resolve, and passes it with `--config`. The installer puts `duckdb.dll`
  in `$INSTDIR`, beside `dbrover.exe`. The uninstaller removes it with the other resources.

**The Visual C++ runtime.** `duckdb.dll` is DuckDB's own MSVC build and imports the dynamic
Visual C++ runtime (`vcruntime140.dll`, `msvcp140.dll` and the like), as does `dbrover.exe`. A
clean Windows doesn't have it, and then DbRover can't start. winget's validation flagged the
launch of the first package's installed copy, the likeliest cause being this. Linking `dbrover.exe`'s runtime statically (`+crt-static`) can't fix
it, because the DLL would still need it. So the installer ships the runtime DLLs beside
`dbrover.exe`, which Microsoft calls app-local deployment:
`packaging/windows/vc-runtime.sh` reads both binaries' imports with `dumpbin`, follows them into
the runner's Visual Studio redistributable folder, and prints the DLLs found there. The
Windows job adds them to the resources like `duckdb.dll`.

A smoke test installs silently (`/S /D=…`), removes every DuckDB folder from `PATH`, checks
that each runtime DLL is in the install folder (the runner has the runtime installed, so
running DbRover can't show one missing), and runs the installed `dbrover.exe --version`. In the spike, the installer held `dbrover.exe` (48.3 MB)
and `duckdb.dll` (36.7 MB).

**Parquet and JSON: included.** The DLL passes the same version and extension test in CI.
The DLL is Authenticode-signed by the Stichting DuckDB Foundation, so signing DbRover's
installer later won't leave an unsigned binary inside it.

### 3. Same DuckDB

`fetch.sh` takes the version from `Cargo.lock`. The test asserts `v1.5.6` against the linked
library, which catches a mismatched pin or a pkg-config surprise. The whole suite passes on
all three OSes against the prebuilt libraries, including `dbrover-driver-duckdb`'s conformance
checks (lifecycle, execution, transactions, describe table, catalog). Locally on macOS, the 13
driver tests pass in release mode against the static library.

### 4. Numbers

Measured in the spike, which ran without sccache, on GitHub's hosted runners:
`ubuntu-latest`, `macos-latest` (macOS 26, arm64) and `windows-latest` for CI; `ubuntu-22.04`,
`macos-latest` and `windows-2022` for packaging. The baseline was `main` with only the
measurement steps added, on a branch of its own. Each branch had its own `rust-cache` prefix,
so the first run was cold. One run each, so expect a minute or two of noise.

| Run | Baseline (compiled) | Prebuilt |
| --- | --- | --- |
| First (cold) | [40199928020](https://github.com/navasvarela/dbrover/actions/runs/40199928020) | [40200493004](https://github.com/navasvarela/dbrover/actions/runs/40200493004) |
| Second | [40208062661](https://github.com/navasvarela/dbrover/actions/runs/40208062661) | [40204848715](https://github.com/navasvarela/dbrover/actions/runs/40204848715) |

The second runs were meant to be warm, but every one of them logged `No cache found`. The
first runs had saved their caches, and GitHub had already evicted them. See
[The cache cap](#the-cache-cap): that is a finding in its own right. A warm number came instead
from rerunning the Linux `check` job right after its cache was saved.

**CI, `check` job** (whole job, then its clippy and test steps):

| OS | Cold, compiled | Cold, prebuilt | Second run, compiled (cold again) | Second run, prebuilt (cold again) |
| --- | --- | --- | --- | --- |
| Linux | 34m56s (14m44s + 17m59s) | **8m39s** (2m24s + 4m42s) | 35m42s (15m08s + 18m28s) | 6m17s (1m30s + 3m00s) |
| macOS | 24m26s (10m44s + 11m57s) | **12m05s** (3m47s + 6m46s) | 24m21s (10m50s + 11m58s) | 9m24s (3m02s + 5m12s) |
| Windows | 37m25s (12m59s + 18m19s) | **14m24s** (4m14s + 7m40s) | 41m02s (14m00s + 19m36s) | 12m03s (3m29s + 6m18s) |

**Warm**, with the cache restored (Linux `check` job, rerun right after its cache was saved;
both logs say `full match: true`):

| | Compiled | Prebuilt |
| --- | --- | --- |
| Whole job | 4m15s | **3m34s** |
| Restoring the cache | 75 s (3.4 GB) | 31 s (1.3 GB) |
| clippy + test | 11 s + 1m41s | 10 s + 1m23s |

When the cache hits, both builds skip DuckDB, so the prebuilt library saves only the time to
restore a cache that is 2 GB larger. The gain is in how often the cache misses.

<a id="the-cache-cap"></a>
**The cache cap.** GitHub keeps at most 10 GB of Actions caches per repository and evicts the
least recently used beyond that. During the spike the repository sat at 10.07 GB with only
four caches. The compiled build's caches are huge, because they hold DuckDB's debug objects
and its build-script output twice:

| Cache (`rust-cache`, one per job and OS) | Compiled | Prebuilt |
| --- | --- | --- |
| CI `check`, Linux | 3,359 MB | 1,336 MB |
| CI `check`, macOS | 2,347 MB | 1,009 MB |
| CI `check`, Windows | 2,963 MB | 1,136 MB |
| **CI, three OSes** | **8,669 MB (87% of the cap)** | **3,481 MB (35%)** |
| Release `linux` / `macos` / `windows` | 788 / 1,128 / 875 MB | 745 / 1,056 / 649 MB |

So with DuckDB compiled, `main`'s three CI caches alone nearly filled the cap. Any pull
request, release or other branch that saved a cache evicted one of `main`'s, and the next run
on `main` was cold: 10 to 14 minutes of DuckDB again. #85 (save only on `main`) reduces the
churn but couldn't make 8.7 GB of caches fit beside anything else. With the prebuilt library,
`main`'s CI caches and the release caches fit together.

A compiled cold run compiles DuckDB twice. The baseline's Linux log shows `libduckdb-sys`
building for 13m20s under clippy, then again for about 14m50s under `cargo test`, so the two
steps don't share its build-script output. A prebuilt run pays for neither.

**Release builds** (the build step only; packaging adds well under a minute):

| Job | Compiled | Prebuilt | Prebuilt, second run (cold again) |
| --- | --- | --- | --- |
| macOS universal | 37m56s (arm64 20m19s, x86_64 17m30s) | **19m57s** (11m26s, 8m19s) | 15m29s (8m39s, 6m41s) |
| Linux | 21m46s | **6m38s** | 8m31s |
| Windows | 19m16s | **9m39s** | 11m54s |
| Whole dry run, end to end | 85m54s | **37m44s** | 33m16s |

All of these are cold builds, and that is what a real release gets. The packaging jobs' cache
keys (`macos`, `linux`, `windows`) are never saved by `main`, and a tag can only restore caches
from its own ref or the default branch. The two prebuilt runs differ by a few minutes either
way: runner noise. What remains in a prebuilt release build is mostly DbRover's own release
profile (thin LTO, one codegen unit).

Local, for reference (Apple-silicon Mac, 12 cores, under heavy load from other builds, so
only the ratio means anything): a clean `cargo build --release -p dbrover-app` took 14m49s
compiled and 3m41s prebuilt.

**Sizes** (stripped release binaries and the packages users download):

| Artefact | Compiled | Prebuilt | Change |
| --- | --- | --- | --- |
| macOS arm64 binary | 74.4 MB | 81.7 MB | +7.3 MB |
| macOS x86_64 binary | 79.6 MB | 87.3 MB | +7.7 MB |
| macOS universal binary | 154.0 MB | 169.0 MB | +15.0 MB |
| **`.dmg`** | **67.0 MB** | **72.7 MB** | **+5.8 MB (+8.6%)** |
| `.app.tar.gz` (update) | 60.2 MB | 65.6 MB | +5.4 MB |
| Linux binary | 91.6 MB | 101.5 MB | +9.9 MB |
| **AppImage** | **32.9 MB** | **36.6 MB** | **+3.6 MB (+11%)** |
| **`.deb`** | **35.4 MB** | **39.4 MB** | **+4.0 MB (+11%)** |
| Windows `dbrover.exe` | 77.5 MB | 48.3 MB + `duckdb.dll` 36.7 MB | +7.5 MB on disk |
| **Windows installer** | **20.7 MB** | **22.5 MB** | **+1.8 MB (+8.8%)** |

Locally on macOS the gzipped arm64 binary went from 28.6 to 31.4 MB.

Where the growth comes from: DuckDB's release libraries carry ICU (with its time-zone and
collation data) and autocomplete, which the generated loader pulls in. On Linux they also
carry jemalloc, DuckDB's default allocator in its own builds. The bundled build has only
`core_functions`, `parquet` and `json` (checked locally: those three are its statically linked
extensions). ICU is partly a gain. Both builds have `autoinstall_known_extensions` and
`autoload_known_extensions` on, so in the bundled build a query that needs ICU (time zones,
`AT TIME ZONE`, ICU collations) downloads the extension from `extensions.duckdb.org`. The
prebuilt one has it offline. Other extensions, such as `httpfs`, still autoinstall in both.

Trimming ICU and autocomplete would need a loader of DbRover's own, compiled with `cc` against
DuckDB's internal C++ API (`duckdb::ExtensionHelper::LoadAllExtensions` and friends). That
would bring a C++ compile back, and a C++ API that can change in any DuckDB release. Not
recommended for 2 to 6 MB.

### 5. Supply chain

- **Source:** only `https://github.com/duckdb/duckdb/releases/download/v<version>/<archive>`,
  the same project that publishes the sources `bundled` compiles. Nothing comes from a
  third-party crate or mirror.
- **Pins:** `packaging/duckdb/SHA256SUMS`, one line per version and archive
  (`<sha256>  v1.5.6/static-libs-osx-arm64.zip`). The file pins the four archives DbRover ships
  with, plus `static-libs-linux-arm64.zip` for ARM Linux developers. A mismatch, or a version
  or target with no pin, fails the build before anything is extracted.
- **Script:** `packaging/duckdb/fetch.sh`, beside the other packaging scripts. It caches by
  version in the directory it is given, re-checks the zip on every run, and re-downloads if
  the check fails.
- **Local builds:** `cargo build` with no flag still compiles DuckDB. Nothing to install, no
  network beyond crates.io. CONTRIBUTING.md shows how to use the prebuilt library locally.

### 6. Caveats

**glibc and libstdc++ (AppImage, ubuntu-22.04).** DuckDB's Linux objects reference old glibc
entry points (`__xstat`, `__fxstat`), so they were built against an old glibc: compatible. The
binary's requirements are set when it is linked on ubuntu-22.04:

| Requirement | Compiled | Prebuilt |
| --- | --- | --- |
| glibc | `GLIBC_2.35` | `GLIBC_2.35` (unchanged) |
| libstdc++ | `GLIBCXX_3.4.29` (GCC 11) | `GLIBCXX_3.4.30` (GCC 12), `CXXABI_1.3.13` |

The new symbol is `std::condition_variable::wait`, at the version GCC 12 gave it. Ubuntu
22.04 ships libstdc++ from GCC 12, and so does every distribution new enough for glibc 2.35
(Debian 12, Fedora 36 and later). In practice no distribution that runs today's AppImage is
lost. The AppImage doesn't bundle libstdc++, in either build.

**macOS minimum version.** The objects in both macOS archives are built with
`LC_BUILD_VERSION minos 11.0` (SDK 26.5), matching DbRover's `minimumSystemVersion = "11.0"`.

**Licences.** DuckDB is MIT (Copyright Stichting DuckDB Foundation), and its third-party code
carries its own licences. The packages ship `licenses/DuckDB-LICENSE`, and
`licenses/DuckDB-THIRD-PARTY.txt` with every licence under `third_party/`, the ICU extension's ICU and
the ALP algorithms in DuckDB's repository at the release tag.
`packaging/duckdb/update-licenses.sh <version>` writes both. Before this change DbRover shipped
no DuckDB licence at all, compiled or not. The prebuilt libraries add ICU (Unicode License v3)
and, on Linux, jemalloc (BSD-2-Clause) to what the compiled build contains; the notices cover
both builds.

**Upgrading DuckDB.**

1. Bump `duckdb` in `Cargo.toml` (crate 1.MMmmpp.x is DuckDB MM.mm.pp).
2. Add the new version's lines to `SHA256SUMS`. GitHub's recorded digests are one command
   away, but download and hash the archives too, rather than copying digests alone:

   ```bash
   gh release view v1.5.7 -R duckdb/duckdb --json assets --jq '.assets[]
     | select(.name | test("^static-libs-(osx|linux)-|^libduckdb-windows-amd64"))
     | "\(.digest | ltrimstr("sha256:"))  v1.5.7/\(.name)"'
   ```

3. Run `packaging/duckdb/update-licenses.sh <version>` and commit what changed.
4. Update the version the test expects (`v1.5.6`).

A crate release can come before DuckDB's assets, or the other way round. Until both exist, the
fetch fails loudly rather than mixing versions. Old pins can stay in the file or go.

**Other caveats.**

- The `RUSTFLAGS` change gave CI a new cache key, so the first run after it was cold.
- `-D warnings` in CI's `RUSTFLAGS` keeps working: the cfg sits beside it.
- The bundled path is no longer built in CI. The version test runs in both modes, and
  `bundled` is upstream's default and best-tested path. A weekly scheduled job could build it,
  if that coverage matters.
- Windows now ships two files. The installer handles that. If DbRover ever ships a portable
  `.zip` for Windows, the DLL must go in it too.
- The fetch downloads 13 to 30 MB per job, which took 2 to 8 seconds in these runs.
  `actions/cache` could keep it, but it isn't worth it.

## Considered and not chosen

- **Third-party `frozen-duckdb` crate:** already rejected (one maintainer, known divergences,
  unverified binaries).
- **The dynamic library everywhere:** `libduckdb-osx-universal.zip` and
  `libduckdb-linux-amd64.zip` exist. On macOS, a `.dylib` inside the `.app` needs an rpath and
  its own code signature and notarisation. On Linux, it needs `RPATH`/AppImage plumbing and
  goes into the `.deb`. The static libraries avoid all of that, with no size advantage to the
  dylib.
- **Keep compiling, and cache the C++ objects with sccache.** #89 added sccache to CI, and the
  cc crate sends DuckDB's C++ through it too. It helped: on `main`'s second run with it,
  rust-cache had been evicted, sccache hit 60% of compiles, and the Linux job took 19.6 minutes
  instead of 37. But its entries go into the same 10 GB of cache, and it does nothing for
  release builds (tags start cold, and release objects differ from CI's debug ones). With the
  prebuilt library, CI's rust-cache fits under the cap and a cold run takes 9–14 minutes
  without sccache, so this change removes it.
- **`DUCKDB_DOWNLOAD_LIB`:** downloads the dynamic library with no checksum, from a build
  script.

## Decision

**Static libraries on macOS and Linux, the DLL on Windows, for CI and releases. A plain
`cargo build` stays `bundled`.**

- Releases go from about 86 to about 38 minutes end to end. The macOS job, the longest, goes
  from 38 to 20 minutes of building.
- A cold CI run goes from 24–37 minutes per OS to 9–14 minutes.
- CI's caches shrink from 8.7 GB to 3.5 GB, so they stop evicting each other under GitHub's
  10 GB cap.
- The cost is 2 to 6 MB per download (about 10%). DuckDB's share of the downloads goes from
  about 13 MB to about 16 MB, still inside the 30 MB budget in `docs/databases.md`. In
  exchange, ICU works offline.
- In complexity: a fetch script with its pins, a 16-line build script, the cfg in two Cargo
  manifests, and the Windows packager config and installer smoke test in `release.yml`.

Going back to compiling everywhere means removing `--cfg dbrover_prebuilt_duckdb` from the two
workflows and the fetch steps; nothing else depends on it.
