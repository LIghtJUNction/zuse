#!/usr/bin/env bash
# Run the compiled Rust capture fixture in new, owned headless compositors.
# No Python, host desktop connection, host accessibility bus or input helper.
# Usage: bash tests/run_native_wayland_capture.sh TEST_BINARY CAGE HYPRLAND [SCALE] [LOG_PARENT] [RENDER_NODE]
# CAGE must provide the headless wl_compositor v6 / wl_seat v9 / xdg-shell v6
# / linux-dmabuf v4 interfaces required by Aquamarine 0.16. Missing support is
# a bounded startup failure. Build the pinned private Cage with
# tests/build_private_cage.sh from a local source archive. Neither script needs
# the older Python harness or installs a system compositor. See
# docs/native-testing.md for the Python-free build and run procedure.
set -Eeuo pipefail
set +m
umask 077
ulimit -c 0

usage() {
    printf '%s\n' 'Usage: run_native_wayland_capture.sh TEST_BINARY CAGE HYPRLAND [1|1.25] [LOG_PARENT] [/dev/dri/renderDNUMBER]'
}
if [[ ${1-} == --help ]]; then usage; exit 0; fi
if (( $# < 3 || $# > 6 )); then usage >&2; exit 2; fi

resolve_executable() {
    local resolved
    resolved=$(command -v -- "$1") || return 1
    resolved=$(realpath -e -- "$resolved") || return 1
    [[ -f $resolved && -x $resolved ]] || return 1
    printf '%s\n' "$resolved"
}
test_binary=$(resolve_executable "$1") || { printf 'test binary is unavailable\n' >&2; exit 2; }
cage_binary=$(resolve_executable "$2") || { printf 'private Cage is unavailable\n' >&2; exit 2; }
hyprland_binary=$(resolve_executable "$3") || { printf 'Hyprland is unavailable\n' >&2; exit 2; }
scale=${4:-1.25}
case "$scale" in 1|1.25) ;; *) printf 'scale must be 1 or 1.25\n' >&2; exit 2;; esac
log_parent=${5:-"$PWD/target/native-wayland-capture"}
render_node=${6:-}
if [[ -n $render_node ]]; then
    render_node=$(realpath -e -- "$render_node")
    [[ $render_node =~ ^/dev/dri/renderD[0-9]+$ && -c $render_node ]] || {
        printf 'render node must be a /dev/dri/renderDNUMBER character device\n' >&2; exit 2;
    }
fi
env_binary=$(resolve_executable env)
setsid_binary=$(resolve_executable setsid)
dbus_binary=$(resolve_executable dbus-daemon)
bash_binary=$(resolve_executable bash)
source_directory=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
config_template="$source_directory/fixtures/native_wayland_capture.lua.in"
bus_template="$source_directory/fixtures/native_wayland_capture_dbus.conf.in"
guard_script="$source_directory/fixtures/native_wayland_capture_guard.sh"
[[ -r $config_template && -r $bus_template && -r $guard_script ]] || { printf 'runner configuration templates or guardian are missing\n' >&2; exit 2; }

mkdir -p -- "$log_parent"
log_parent=$(realpath -e -- "$log_parent")
log_directory=$(mktemp -d -- "$log_parent/run-XXXXXX")
private_directory=$(mktemp -d -- /tmp/zc-XXXXXX)
private_runtime="$private_directory/r"
stage=prepare
declare -a owned_pids=() owned_starts=() owned_labels=()
declare -a command_pids=() command_starts=() status_files=()
last_pid=0
cleanup_failed=0

process_snapshot() {
    local pid=$1 line tail
    local -a fields
    [[ -r /proc/$pid/stat ]] || return 1
    IFS= read -r line < "/proc/$pid/stat" || return 1
    tail=${line##*) }
    read -r -a fields <<< "$tail"
    (( ${#fields[@]} >= 20 )) || return 1
    observed_state=${fields[0]}
    observed_group=${fields[2]}
    observed_session=${fields[3]}
    observed_start=${fields[19]}
}
command_alive() {
    local index=$1
    [[ ! -s ${status_files[index]} ]] &&
        process_snapshot "${command_pids[index]}" &&
        [[ $observed_start == "${command_starts[index]}" && $observed_state != Z && $observed_state != X ]]
}
group_alive() {
    local index=$1 entry pid=${owned_pids[$1]}
    if owned_alive "$index"; then return 0; fi
    for entry in /proc/[0-9]*/stat; do
        entry=${entry%/stat}
        if process_snapshot "${entry##*/}" && [[ $observed_session == "$pid" && $observed_state != Z && $observed_state != X ]]; then
            return 0
        fi
    done
    return 1
}
owned_alive() {
    local index=$1
    process_snapshot "${owned_pids[index]}" &&
        [[ $observed_start == "${owned_starts[index]}" && $observed_state != Z && $observed_state != X ]]
}
signal_owned() {
    local index=$1 signal=$2 pid=${owned_pids[$1]}
    if owned_alive "$index"; then
        if [[ $observed_group == "$pid" ]]; then
            kill -s "$signal" -- "-$pid" 2>/dev/null || true
        else
            # Before setsid has exec'd, only our recorded child can be signalled.
            kill -s "$signal" -- "$pid" 2>/dev/null || true
        fi
    fi
}
cleanup() {
    local index deadline
    for (( index=${#owned_pids[@]}-1; index>=0; index-- )); do
        signal_owned "$index" TERM
        deadline=$((SECONDS+3))
        while group_alive "$index" && (( SECONDS < deadline )); do sleep 0.05; done
        if owned_alive "$index"; then
            signal_owned "$index" KILL
            deadline=$((SECONDS+3))
            while group_alive "$index" && (( SECONDS < deadline )); do sleep 0.05; done
        fi
        if group_alive "$index"; then
            printf 'cleanup did not finish: %s pid=%s\n' "${owned_labels[index]}" "${owned_pids[index]}" >&2
            cleanup_failed=1
        else
            wait "${owned_pids[index]}" 2>/dev/null || true
        fi
    done
}
finish() {
    local status=$1 cleanup_log_fd verdict
    trap - EXIT INT TERM
    set +e
    # Opening a diagnostic log must never be a prerequisite for stopping our
    # children. Keep the descriptor open so a second path lookup cannot skip
    # cleanup after the successful open.
    if exec {cleanup_log_fd}>> "$log_directory/cleanup.log"; then
        cleanup 2>&"$cleanup_log_fd"
        exec {cleanup_log_fd}>&-
    else
        printf 'cleanup log is unavailable; stopping owned processes without it\n' >&2
        cleanup
        status=125
    fi
    if (( cleanup_failed != 0 )); then status=125; fi
    if (( status == 0 )); then
        verdict=passed
    else
        verdict="blocked stage=$stage exit=$status"
    fi
    if ! printf '%s\n' "$verdict" > "$log_directory/status"; then
        status=125
        verdict="blocked stage=$stage exit=$status status-log-unavailable"
    fi
    # This is the single directory allocated by this process, never a supplied
    # directory or the host XDG_RUNTIME_DIR. Logs are retained separately.
    if (( cleanup_failed == 0 )); then
        rm -rf -- "$private_directory"
    else
        printf 'unresolved owned-session state retained at: %s\n' "$private_directory" >&2
    fi
    printf 'native capture gate: %s; logs: %s\n' "$verdict" "$log_directory"
    exit "$status"
}
trap 'finish $?' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -m 700 -- "$private_runtime" "$private_directory/config" "$private_directory/cache" \
    "$private_directory/data" "$private_directory/state" "$private_directory/empty-config" \
    "$private_directory/empty-data" "$private_directory/tmp"
# The 40-character revision plus timestamp/random suffix must fit sun_path.
longest_ipc="$private_runtime/hypr/xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx_9999999999_9999999999/.socket2.sock"
(( ${#longest_ipc} < 108 )) || { printf 'private IPC path is too long\n' >&2; exit 2; }
sed "s/@SCALE@/$scale/g" "$config_template" > "$private_directory/hyprland.lua"
sed "s|@RUNTIME@|$private_runtime|g" "$bus_template" > "$private_directory/dbus.conf"
cp -- "$private_directory/hyprland.lua" "$log_directory/hyprland.lua"

# HOME is preserved exactly, never redirected. All XDG state and service
# endpoints belong to this disposable fixture; no other host variables survive.
common_environment=(
    "PATH=$PATH" "HOME=${HOME:?}" "LANG=C.UTF-8" "TERM=dumb"
    "XDG_RUNTIME_DIR=$private_runtime" "XDG_CONFIG_HOME=$private_directory/config"
    "XDG_CACHE_HOME=$private_directory/cache" "XDG_DATA_HOME=$private_directory/data"
    "XDG_STATE_HOME=$private_directory/state"
    "XDG_CONFIG_DIRS=$private_directory/empty-config" "XDG_DATA_DIRS=$private_directory/empty-data"
    "TMPDIR=$private_directory/tmp" "XDG_CURRENT_DESKTOP=zuse-private-headless" "XDG_SESSION_TYPE=wayland"
    "HYPRLAND_NO_SD_VARS=1" "HYPRLAND_NO_SD_NOTIFY=1" "HYPRLAND_NO_RT=1" "HYPRLAND_NO_CRASHREPORTER=1"
    "AQ_DRM_DEVICES=/dev/null" "LIBSEAT_BACKEND=seatd" "SEATD_SOCK=$private_runtime/disabled-seatd.sock"
    "YDOTOOL_SOCKET=$private_runtime/disabled-ydotool.sock"
    "DBUS_SYSTEM_BUS_ADDRESS=unix:path=$private_runtime/disabled-system-bus.sock"
    "AT_SPI_BUS_ADDRESS=unix:path=$private_runtime/disabled-atspi.sock"
    "NO_AT_BRIDGE=1" "GTK_A11Y=none" "GTK_USE_PORTAL=0" "GIO_USE_VFS=local" "GSETTINGS_BACKEND=memory"
)

launch() {
    local label=$1 stdout_path=$2 stderr_path=$3 pid descriptor path index prefix deadline command_pid command_start
    local -n child_environment=$4
    shift 4
    index=${#owned_pids[@]}
    prefix="$private_directory/owner-$index"
    (
        # The current shell owns these inherited descriptors. Close everything
        # beyond stdio before exec, including any inherited host display fd.
        for path in /proc/self/fd/*; do
            descriptor=${path##*/}
            if [[ $descriptor =~ ^[0-9]+$ ]] && (( descriptor > 2 )); then
                exec {descriptor}>&- || true
            fi
        done
        exec "$setsid_binary" -- "$env_binary" -i "${child_environment[@]}" "$bash_binary" "$guard_script" "$prefix" "$@"
    ) < /dev/null > "$stdout_path" 2> "$stderr_path" &
    pid=$!
    process_snapshot "$pid" || { wait "$pid" || true; printf '%s did not start\n' "$label" >&2; return 1; }
    owned_pids+=("$pid") owned_starts+=("$observed_start") owned_labels+=("$label")
    printf '%s pid=%s start_ticks=%s\n' "$label" "$pid" "$observed_start" >> "$log_directory/owners"
    status_files+=("$prefix.status")
    deadline=$((SECONDS+3))
    while [[ ! -s $prefix.child ]]; do
        owned_alive "$index" && (( SECONDS < deadline )) || { printf '%s guardian did not report its child\n' "$label" >&2; return 1; }
        sleep 0.02
    done
    IFS=' ' read -r command_pid command_start < "$prefix.child"
    [[ $command_pid =~ ^[0-9]+$ && $command_start =~ ^[0-9]+$ ]] || return 1
    command_pids+=("$command_pid")
    command_starts+=("$command_start")
    last_pid=$command_pid
    printf '%s command_pid=%s start_ticks=%s\n' "$label" "$command_pid" "$command_start" >> "$log_directory/owners"
    owned_alive "$index" && [[ $observed_group == "$pid" && $observed_session == "$pid" ]]
}
assert_no_physical_devices() {
    local index=$1 fd target pid=${command_pids[$1]}
    command_alive "$index" || { printf 'owned compositor exited: %s\n' "$pid" >&2; return 1; }
    for fd in /proc/"$pid"/fd/*; do
        target=$(readlink -- "$fd") || continue
        case "$target" in
            /dev/input/*|/dev/dri/card*|/dev/tty*|/dev/uinput*)
                printf 'private compositor %s opened forbidden device %s\n' "$pid" "$target" >&2; return 1;;
        esac
    done
    command_alive "$index"
}
assert_private_socket() {
    local socket=$1 canonical
    [[ -S $socket ]] || return 1
    canonical=$(realpath -e -- "$socket") || return 1
    [[ $canonical == "$private_runtime/"* ]] || return 1
}

stage=private-dbus
launch dbus "$private_directory/bus-address" "$log_directory/dbus.log" \
    common_environment "$dbus_binary" "--config-file=$private_directory/dbus.conf" --nofork --print-address=1
deadline=$((SECONDS+3))
while [[ ! -s $private_directory/bus-address ]]; do
    command_alive 0 && (( SECONDS < deadline )) || { printf 'private D-Bus did not become ready\n' >&2; exit 1; }
    sleep 0.05
done
IFS= read -r bus_address < "$private_directory/bus-address"
[[ $bus_address == "unix:path=$private_runtime/dbus.sock"* ]] || { printf 'unexpected private D-Bus address\n' >&2; exit 1; }
assert_private_socket "$private_runtime/dbus.sock"
common_environment+=("DBUS_SESSION_BUS_ADDRESS=$bus_address")

stage=headless-cage
parent_environment=("${common_environment[@]}" "WLR_BACKENDS=headless" "WLR_HEADLESS_OUTPUTS=1"
    "WLR_RENDERER=gles2" "WLR_DRM_DEVICES=/dev/null" "WLR_XWAYLAND=/bin/false")
if [[ -n $render_node ]]; then parent_environment+=("WLR_RENDER_DRM_DEVICE=$render_node"); fi
launch cage "$log_directory/cage.log" "$log_directory/cage.err" parent_environment "$cage_binary" -D
cage_pid=$last_pid
deadline=$((SECONDS+12))
shopt -s nullglob
while :; do
    displays=()
    for candidate in "$private_runtime"/wayland-*; do [[ ! -S $candidate ]] || displays+=("$candidate"); done
    (( ${#displays[@]} <= 1 )) || { printf 'private Cage display is ambiguous\n' >&2; exit 1; }
    if (( ${#displays[@]} == 1 )); then parent_socket=${displays[0]}; break; fi
    command_alive 1 && (( SECONDS < deadline )) || { printf 'headless Cage failed or timed out; no host fallback\n' >&2; exit 1; }
    sleep 0.05
done
assert_private_socket "$parent_socket"
assert_no_physical_devices 1

stage=private-hyprland
hyprland_environment=("${common_environment[@]}" "WAYLAND_DISPLAY=$parent_socket")
launch hyprland "$log_directory/hyprland.log" "$log_directory/hyprland.err" \
    hyprland_environment "$hyprland_binary" --config "$private_directory/hyprland.lua"
hyprland_pid=$last_pid
deadline=$((SECONDS+12))
while :; do
    ipcs=() displays=()
    for candidate in "$private_runtime"/hypr/*/.socket.sock; do [[ ! -S $candidate ]] || ipcs+=("$candidate"); done
    for candidate in "$private_runtime"/wayland-*; do
        if [[ -S $candidate && $candidate != "$parent_socket" ]]; then displays+=("$candidate"); fi
    done
    (( ${#ipcs[@]} <= 1 && ${#displays[@]} <= 1 )) || { printf 'private Hyprland sockets are ambiguous\n' >&2; exit 1; }
    if (( ${#ipcs[@]} == 1 && ${#displays[@]} == 1 )); then break; fi
    command_alive 1 && command_alive 2 && (( SECONDS < deadline )) || {
        printf 'private Hyprland failed or timed out; check Cage protocol/GL support; no host fallback\n' >&2; exit 1;
    }
    sleep 0.05
done
assert_private_socket "${ipcs[0]}"
assert_private_socket "${displays[0]}"
signature=${ipcs[0]%/.socket.sock}
signature=${signature##*/}
assert_no_physical_devices 1
assert_no_physical_devices 2

stage=native-rust-fixture
fixture_environment=("${common_environment[@]}" "WAYLAND_DISPLAY=${displays[0]##*/}"
    "HYPRLAND_INSTANCE_SIGNATURE=$signature" "ZUSE_BACKEND=wayland" "GDK_BACKEND=wayland"
    "ZUSE_PRIVATE_WAYLAND_FIXTURE=1" "ZUSE_PRIVATE_WAYLAND_HEADLESS=1" "ZUSE_NATIVE_WINDOW_CAPTURE_FIXTURE=1")
launch rust-fixture "$log_directory/test.log" "$log_directory/test.err" fixture_environment \
    "$test_binary" --ignored --exact native_wayland_capture_preserves_window_identity_across_focus_and_close \
    --nocapture --test-threads=1 --color=never
fixture_pid=$last_pid
deadline=$((SECONDS+60))
while [[ ! -s ${status_files[3]} ]]; do
    owned_alive 3 && command_alive 1 && command_alive 2 || { printf 'a private process exited during capture validation\n' >&2; exit 1; }
    (( SECONDS < deadline )) || { printf 'native capture fixture exceeded 60 seconds\n' >&2; exit 124; }
    sleep 0.05
done
IFS= read -r fixture_status < "${status_files[3]}"
[[ $fixture_status =~ ^[0-9]+$ ]] || { printf 'invalid fixture exit status\n' >&2; exit 1; }
(( fixture_status == 0 )) || { printf 'native Rust fixture failed: %s\n' "$fixture_status" >&2; exit "$fixture_status"; }
assert_no_physical_devices 1
assert_no_physical_devices 2
matched=false
while IFS= read -r line; do
    case "$line" in *'test result: ok. 1 passed;'*) matched=true;; esac
done < "$log_directory/test.log"
[[ $matched == true ]] || { printf 'fixture did not run exactly one passing test\n' >&2; exit 1; }
printf 'private compositor sockets verified; physical input/KMS fds: none\n' > "$log_directory/isolation"
stage=complete
