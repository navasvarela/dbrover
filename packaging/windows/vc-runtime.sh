#!/usr/bin/env bash
# Prints the Visual C++ runtime DLLs the given Windows binaries need, one path per line, taken
# from the redistributable folder of this machine's Visual Studio (see docs/duckdb-prebuilt.md):
#
#   packaging/windows/vc-runtime.sh target/release/dbrover.exe "$DUCKDB_LIB_DIR/duckdb.dll"
#
# The installer ships them beside dbrover.exe, which Microsoft calls app-local deployment,
# because a clean Windows doesn't have them: dbrover.exe then fails to start with
# STATUS_DLL_NOT_FOUND. Linking dbrover.exe's runtime statically (`+crt-static`) wouldn't be
# enough, since duckdb.dll, DuckDB's own MSVC build, imports the runtime too.
#
# Runs on a GitHub Windows runner, in Git Bash, where vswhere has a fixed path.
set -euo pipefail

[[ $# -ge 1 ]] || { echo "usage: $0 <binary>..." >&2; exit 2; }

vswhere="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
visual_studio=$(cygpath -u "$("$vswhere" -latest -products '*' -property installationPath | tr -d '\r')")
dumpbin=$(cygpath -u "$("$vswhere" -latest -products '*' \
    -find 'VC/Tools/MSVC/**/bin/Hostx64/x64/dumpbin.exe' | tr -d '\r' | head -n 1)")
# The newest numbered version; the folder also holds non-numbered ones such as v143.
runtime_dir=$(ls -d "$visual_studio"/VC/Redist/MSVC/[0-9]*/x64/Microsoft.VC14*.CRT | sort -V | tail -n 1)

imports() {
    "$dumpbin" -nologo -dependents "$1" | tr -d '\r' | sed -n 's/^ *\([^ ]*\.[dD][lL][lL]\)$/\1/p' |
        tr '[:upper:]' '[:lower:]'
}

# The runtime DLLs import one another (msvcp140.dll imports vcruntime140.dll), so the list is
# followed until it stops growing.
shipped=()
pending=("$@")
while [[ ${#pending[@]} -gt 0 ]]; do
    binary=${pending[0]}
    pending=("${pending[@]:1}")
    for dll in $(imports "$binary"); do
        [[ -f "$runtime_dir/$dll" ]] || continue
        [[ " ${shipped[*]} " == *" $dll "* ]] && continue
        shipped+=("$dll")
        pending+=("$runtime_dir/$dll")
    done
done

# Every MSVC build imports vcruntime140.dll, so an empty list means dumpbin's output wasn't
# understood, not that nothing is needed.
[[ ${#shipped[@]} -gt 0 ]] || {
    echo "found no Visual C++ runtime DLL among the imports of $*" >&2
    exit 1
}
for dll in "${shipped[@]}"; do
    cygpath -m "$runtime_dir/$dll"
done
