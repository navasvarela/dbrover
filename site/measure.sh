#!/usr/bin/env bash
# Measures how fast DbRover starts on a Mac and how big its download and install are, with
# DBeaver Community, DataGrip and DBX measured the same way for reference. The method, the
# machine and the results are in docs/measurements.md.
#
#   site/measure.sh setup   APP        # first launch on a throwaway configuration, by hand
#   site/measure.sh startup APP [RUNS] # launch RUNS times (default 5), median times
#   site/measure.sh sizes              # download and install sizes of every app
#
# APP is dbrover, dbeaver, datagrip or dbx, installed in /Applications. Each runs on its own
# configuration under MEASURE_DIR (default: $TMPDIR/dbrover-measure), never on the user's own,
# so `setup` shows each app's first-run screens once. Results are appended to
# MEASURE_DIR/results.tsv.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
probe="${root}/site/window-probe.js"
measure_dir="${MEASURE_DIR:-${TMPDIR:-/tmp}/dbrover-measure}"
results="${measure_dir}/results.tsv"

usage() {
    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

app_path() {
    case "$1" in
        dbrover) echo /Applications/DbRover.app ;;
        dbeaver) echo /Applications/DBeaver.app ;;
        datagrip) echo /Applications/DataGrip.app ;;
        dbx) echo /Applications/DBX.app ;;
        *) usage ;;
    esac
}

plist_value() {
    defaults read "$(app_path "$1")/Contents/Info" "$2"
}

# The `open` arguments that point each app at its throwaway configuration.
#  - DbRover keeps its files under $HOME (~/.config/dbrover, ~/.local/share/dbrover), so a home of
#    its own is a clean start. Saved passwords still go to the login keychain.
#  - DBX runs on the user's own home: it needs the login keychain as it starts, and macOS finds
#    that keychain through $HOME. Its folder (~/Library/Application Support/com.dbx.app) is
#    empty on a machine where DBX was just installed for measuring, which is what setup expects.
#  - DBeaver keeps its settings in the Eclipse workspace that -data names.
#  - DataGrip reads the folders it uses from the file DATAGRIP_PROPERTIES names.
open_args() {
    local app="$1" dir="${measure_dir}/$1"
    case "${app}" in
        dbrover) echo "--env" "HOME=${dir}/home" ;;
        dbeaver) echo "--args" "-data" "${dir}/workspace" ;;
        datagrip) echo "--env" "DATAGRIP_PROPERTIES=${dir}/idea.properties" ;;
    esac
}

prepare() {
    local app="$1" dir="${measure_dir}/$1"
    mkdir -p "${dir}"
    case "${app}" in
        dbrover) mkdir -p "${dir}/home" ;;
        dbeaver) mkdir -p "${dir}/workspace" ;;
        datagrip)
            cat > "${dir}/idea.properties" <<EOF
idea.config.path=${dir}/config
idea.system.path=${dir}/system
idea.plugins.path=${dir}/plugins
idea.log.path=${dir}/log
EOF
            ;;
    esac
}

# Ends the app and everything it started: SIGTERM, which every app measured handles by saving
# and quitting without asking, then SIGKILL if it is still there after 30 seconds.
quit_app() {
    local pid="$1" waited=0
    pkill -TERM -P "${pid}" 2>/dev/null || true
    kill -TERM "${pid}" 2>/dev/null || true
    while kill -0 "${pid}" 2>/dev/null; do
        if [ "${waited}" -ge 30 ]; then
            kill -KILL "${pid}" 2>/dev/null || true
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done
    sleep 2
}

launch() {
    local app="$1"
    # shellcheck disable=SC2046 # open_args prints separate words on purpose
    osascript -l JavaScript "${probe}" "$(plist_value "${app}" CFBundleIdentifier)" \
        $(open_args "${app}")
}

json_field() {
    sed -n "s/.*\"$2\":\([0-9]*\).*/\1/p" <<<"$1"
}

median() {
    sort -n | awk '{ v[NR] = $1 } END { print (NR % 2) ? v[(NR + 1) / 2] : int((v[NR / 2] + v[NR / 2 + 1]) / 2) }'
}

record() {
    mkdir -p "${measure_dir}"
    [ -f "${results}" ] || printf 'date\tapp\tversion\tmeasure\tvalue\tunit\n' > "${results}"
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$1" \
        "$(plist_value "$1" CFBundleShortVersionString)" "$2" "$3" "$4" | tee -a "${results}"
}

cmd_setup() {
    local app="$1"
    prepare "${app}"
    echo "Take ${app} through its first-run screens to its main window, then quit it (⌘Q)."
    # shellcheck disable=SC2046 # open_args prints separate words on purpose
    open -n -W -b "$(plist_value "${app}" CFBundleIdentifier)" $(open_args "${app}")
    echo "${app} is set up."
}

cmd_startup() {
    local app="$1" runs="${2:-5}" result windows=() quiets=()
    [ -d "${measure_dir}/${app}" ] || { echo "run '$0 setup ${app}' first" >&2; exit 1; }
    for ((run = 1; run <= runs; run++)); do
        result="$(launch "${app}")"
        quit_app "$(json_field "${result}" pid)"
        windows+=("$(json_field "${result}" window_ms)")
        quiets+=("$(json_field "${result}" quiet_ms)")
        echo "run ${run}: window ${windows[run - 1]} ms, quiet ${quiets[run - 1]} ms" >&2
    done
    record "${app}" startup_window_median "$(printf '%s\n' "${windows[@]}" | median)" ms
    record "${app}" startup_quiet_median "$(printf '%s\n' "${quiets[@]}" | median)" ms
}

# The size of the file each project offers Apple-silicon Macs, as its server reports it.
# DbRover ships one universal disk image; the others ship one per architecture.
download_url() {
    local version
    version="$(plist_value "$1" CFBundleShortVersionString)"
    case "$1" in
        dbrover) echo "https://github.com/navasvarela/dbrover/releases/download/v${version}/dbrover-${version}-macos-universal.dmg" ;;
        dbeaver) echo "https://dbeaver.io/files/${version}/dbeaver-ce-${version}-macos-aarch64.dmg" ;;
        datagrip) echo "https://download.jetbrains.com/datagrip/datagrip-${version}-aarch64.dmg" ;;
        dbx) echo "https://github.com/t8y2/dbx/releases/download/v${version}/DBX_${version}_arm64.dmg" ;;
    esac
}

cmd_sizes() {
    local app bytes
    for app in dbrover dbeaver datagrip dbx; do
        bytes="$(curl -sIL "$(download_url "${app}")" | tr -d '\r' |
            awk 'tolower($1) == "content-length:" { n = $2 } END { print n }')"
        record "${app}" download_size "$((bytes / 1048576))" MiB
        record "${app}" install_size "$(($(du -sk "$(app_path "${app}")" | cut -f1) / 1024))" MiB
    done
}

case "${1:-}" in
    setup) [ $# -eq 2 ] || usage; app_path "$2" >/dev/null; cmd_setup "$2" ;;
    startup) [ $# -ge 2 ] || usage; app_path "$2" >/dev/null; cmd_startup "$2" "${3:-5}" ;;
    sizes) cmd_sizes ;;
    *) usage ;;
esac
