#!/usr/bin/env bash
# Build the reviewed private Cage fixture from a local archive only.
# No Python, downloads, system installation, Meson or compositor launch.
set -Eeuo pipefail
umask 077
ulimit -c 0
export LC_ALL=C

SOURCE_URL=https://codeload.github.com/cage-kiosk/cage/tar.gz/refs/tags/v0.3.1
SOURCE_SHA256=6dc1619665acd367e0174c93b234002549a66f55f1de9197d67f0305415babc8
SOURCE_COMMIT=b7b774ac957b6d8c7b39f0dcb9ebe19de3e8a45d
ORIGINAL_SHA256=fa2fa2ca03b657216b350728e02f716a043f4f66ef6c76f727b5b484d931611c
PATCHED_SHA256=9ffd623b192a9c9830f670757f0c8559069e3f5de38e9de5f52ccecebbf5022c
OLD='wlr_xdg_shell_create(server.wl_display, 5)'
NEW='wlr_xdg_shell_create(server.wl_display, 6)'

usage() {
    printf '%s\n' 'Usage: build_private_cage.sh --source LOCAL_CAGE_V0.3.1_TAR_GZ --output NEW_PRIVATE_DIRECTORY'
}
fail() { failure=$*; printf 'private Cage build: %s\n' "$failure" >&2; exit 1; }
archive_argument=
output_argument=
while (( $# )); do
    case "$1" in
        --source) (( $# >= 2 )) || { usage >&2; exit 2; }; [[ -z $archive_argument ]] || exit 2; archive_argument=$2; shift 2;;
        --output) (( $# >= 2 )) || { usage >&2; exit 2; }; [[ -z $output_argument ]] || exit 2; output_argument=$2; shift 2;;
        --help|-h) usage; exit 0;;
        *) usage >&2; exit 2;;
    esac
done
[[ -n $archive_argument && -n $output_argument ]] || { usage >&2; exit 2; }

for required in cc pkg-config git tar gzip sha256sum realpath dirname mkdir cp mv rm \
    cat sed awk diff jq timeout; do
    command -v -- "$required" >/dev/null || fail "required installed tool is missing: $required"
done
[[ $(tar --version) == 'tar (GNU tar)'* ]] || fail 'this reviewed builder requires GNU tar'
archive=$(realpath -e -- "$archive_argument") || fail 'source archive is unavailable'
[[ -f $archive ]] || fail 'source must be a regular file'
digest() { local result; result=$(sha256sum < "$1") || return; printf '%s\n' "${result%% *}"; }
[[ $(digest "$archive") == "$SOURCE_SHA256" ]] || fail 'source is not the pinned, unmodified Cage v0.3.1 archive'
[[ ! -e $output_argument && ! -L $output_argument ]] || fail 'output directory must not exist'
output=$(realpath -m -- "$output_argument")
mkdir -p -- "$(dirname -- "$output")"
mkdir -- "$output" || fail 'output directory must be new'

receipt="$output/receipt.json"
build_log="$output/build.log"
stage=initialize
failure=
complete=false
command_stdout=
receipt_update() {
    local filter=$1
    shift
    jq "$@" "$filter" "$receipt" > "$output/receipt.next.json" &&
        mv -- "$output/receipt.next.json" "$receipt"
}
finish() {
    local status=$1
    trap - EXIT INT TERM HUP
    set +e
    if [[ $complete != true || $status != 0 ]]; then
        (( status != 0 )) || status=1
        failure=${failure:-"stage $stage failed with exit $status; see build.log"}
        if [[ -f $receipt ]]; then
            receipt_update '. + {status:"failed", stage:$stage, error:$error}' \
                --arg stage "$stage" --arg error "$failure" || status=125
        fi
    fi
    if [[ -f $receipt ]]; then cat -- "$receipt" || status=125; fi
    exit "$status"
}
trap 'finish $?' EXIT
trap 'failure="interrupted"; exit 130' INT
trap 'failure="terminated"; exit 143' TERM
trap 'failure="hangup"; exit 129' HUP

jq -n --arg url "$SOURCE_URL" --arg source_sha "$SOURCE_SHA256" \
    --arg commit "$SOURCE_COMMIT" --arg old "$OLD" --arg new "$NEW" \
    --arg before "$ORIGINAL_SHA256" --arg after "$PATCHED_SHA256" '
    {status:"building", source_url:$url, source_sha256:$source_sha,
     upstream_version:"0.3.1", upstream_commit:$commit,
     patch:{file:"cage.c", old:$old, new:$new, before_sha256:$before, after_sha256:$after},
     system_install:false, downloads:false, compositor_launched:false}' > "$receipt"
: > "$build_log"

# Preserve the build environment, excluding every desktop endpoint stripped by
# the original builder. The probe has no socket, backend, renderer or seat.
unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET HYPRLAND_INSTANCE_SIGNATURE
run() {
    local status=0
    printf '$' >> "$build_log"
    printf ' %q' "$@" >> "$build_log"
    printf '\n' >> "$build_log"
    timeout --signal=TERM --kill-after=5s 180s "$@" \
        < /dev/null > "$output/command.stdout" 2> "$output/command.stderr" || status=$?
    cat -- "$output/command.stdout" "$output/command.stderr" >> "$build_log"
    command_stdout=$(<"$output/command.stdout")
    # Python str.strip() was used for these ASCII tool responses.
    command_stdout=${command_stdout#"${command_stdout%%[![:space:]]*}"}
    command_stdout=${command_stdout%"${command_stdout##*[![:space:]]}"}
    (( status == 0 )) || fail "stage $stage: command failed with exit $status; see build.log"
}

# Literal shlex-style argv parsing for pkg-config output. This performs no
# variable/command/glob expansion and never evaluates text as shell code.
split_flags() {
    local text=$1 token= state=plain present=false ch next index
    pkg_flags=()
    for (( index=0; index<${#text}; index++ )); do
        ch=${text:index:1}
        case "$state" in
            single)
                if [[ $ch == "'" ]]; then state=plain; else token+=$ch; fi;;
            double)
                case "$ch" in
                    '"') state=plain;;
                    '\')
                        (( index+1 < ${#text} )) || fail 'unfinished escape in pkg-config output'
                        (( index+=1 )); next=${text:index:1}
                        [[ $next == '"' || $next == '\' ]] || token+='\'
                        token+=$next;;
                    *) token+=$ch;;
                esac;;
            plain)
                case "$ch" in
                    ' '|$'\t'|$'\r'|$'\n')
                        if [[ $present == true ]]; then pkg_flags+=("$token"); token=; present=false; fi;;
                    "'") state=single; present=true;;
                    '"') state=double; present=true;;
                    '\')
                        (( index+1 < ${#text} )) || fail 'unfinished escape in pkg-config output'
                        (( index+=1 )); token+=${text:index:1}; present=true;;
                    *) token+=$ch; present=true;;
                esac;;
        esac
    done
    [[ $state == plain ]] || fail 'unclosed quote in pkg-config output'
    if [[ $present == true ]]; then pkg_flags+=("$token"); fi
}
declare -a pkg_flags

stage=check-wlroots
run pkg-config --modversion wlroots-0.20
wlroots_version=$command_stdout
[[ $wlroots_version =~ ^0\.20\.[0-9]+([.+~-].*)?$ ]] || fail 'installed wlroots must be 0.20.x'
run pkg-config --version
pkg_config_version=$command_stdout
run cc --version
compiler_version=${command_stdout%%$'\n'*}
receipt_update '. + {wlroots_version:$wlroots, tool_versions:{pkg_config:$pkg, compiler:$cc}}' \
    --arg wlroots "$wlroots_version" --arg pkg "$pkg_config_version" --arg cc "$compiler_version"

stage=compile-xdg-v6-probe
cat > "$output/xdg_v6_probe.c" <<'C'
#define WLR_USE_UNSTABLE
#include <stdio.h>
#include <wayland-server-core.h>
#include <wlr/types/wlr_xdg_shell.h>

int main(void) {
    struct wl_display *display = wl_display_create();
    if (!display) return 1;
    struct wlr_xdg_shell *shell = wlr_xdg_shell_create(display, 6);
    if (!shell || shell->version != 6 || wl_global_get_version(shell->global) != 6) return 2;
    puts("installed wlroots created an XDG-shell v6 global without a socket, backend, renderer or seat");
    wl_display_destroy(display);
    return 0;
}
C
run pkg-config --cflags --libs wlroots-0.20 wayland-server
split_flags "$command_stdout"
run cc -std=c11 "$output/xdg_v6_probe.c" -o "$output/xdg-v6-probe" "${pkg_flags[@]}"
stage=run-xdg-v6-probe
run "$output/xdg-v6-probe"
receipt_update '. + {installed_library_probe:$probe}' --arg probe "$command_stdout"

stage=verify-archive
# A private snapshot prevents a later input-path replacement changing the bytes
# between hash verification and extraction. Both hashes must match the pin.
cp -- "$archive" "$output/input.tar.gz"
[[ $(digest "$output/input.tar.gz") == "$SOURCE_SHA256" ]] || fail 'archive changed before the private snapshot'
printf '$ gzip -dc -- %q > %q\n' "$output/input.tar.gz" "$output/source.tar" >> "$build_log"
if ! timeout --signal=TERM --kill-after=5s 180s gzip -dc -- "$output/input.tar.gz" \
    > "$output/source.tar" 2> "$output/command.stderr"; then
    cat -- "$output/command.stderr" >> "$build_log"
    fail 'could not decompress the pinned source archive'
fi
cat -- "$output/command.stderr" >> "$build_log"

# Git's native reader validates the git-archive global PAX commit comment.
# Read the private file directly: a gzip pipeline can otherwise see SIGPIPE
# when get-tar-commit-id exits after reading the initial archive records.
printf '$ git get-tar-commit-id < %q\n' "$output/source.tar" >> "$build_log"
if ! archive_commit=$(timeout --signal=TERM --kill-after=5s 180s git get-tar-commit-id \
    < "$output/source.tar" 2> "$output/command.stderr"); then
    cat -- "$output/command.stderr" >> "$build_log"
    fail 'source archive has no valid git-archive PAX commit'
fi
printf '%s\n' "$archive_commit" >> "$build_log"
cat -- "$output/command.stderr" >> "$build_log"
[[ $archive_commit == "$SOURCE_COMMIT" ]] || fail 'source archive PAX comment does not match the pinned commit'

run tar --list --verbose --numeric-owner --full-time --quoting-style=escape --file "$output/source.tar"
mv -- "$output/command.stdout" "$output/archive-members.txt"
# The pinned source uses ordinary ASCII names. Reject quoting, whitespace,
# links, devices, parent traversal and archives outside the original bounds.
awk '
    { count++; total+=$3;
      if (NF!=6 || substr($1,1,1)!~/^[-d]$/ || $3!~/^[0-9]+$/) exit 1;
      name=$6; if (name!~/^cage-0\.3\.1\// || name~/[^A-Za-z0-9_.\/+@-]/) exit 1;
      parts=split(name,segments,"/"); for(i=1;i<=parts;i++) if(segments[i]=="..") exit 1;
      if(count>100 || total>1000000) exit 1; }
    END { if(count<1) exit 1 }
' "$output/archive-members.txt" || fail 'archive contains unexpected paths, types, counts or sizes'
source_directory="$output/source"
mkdir -- "$source_directory"
run tar --extract --file "$output/source.tar" --directory "$source_directory" \
    --strip-components=1 --no-same-owner --no-same-permissions --keep-old-files

stage=patch-xdg-shell-version
cage_source="$source_directory/cage.c"
[[ $(digest "$cage_source") == "$ORIGINAL_SHA256" ]] || fail 'Cage source does not match the reviewed file'
count=$(awk -v needle="$OLD" '{text=$0; while((at=index(text,needle))>0){count++; text=substr(text,at+length(needle));}} END{print count+0}' "$cage_source")
[[ $count == 1 ]] || fail 'expected exactly one XDG-shell version selection'
cp -- "$cage_source" "$output/cage.c.before"
sed 's/wlr_xdg_shell_create(server\.wl_display, 5)/wlr_xdg_shell_create(server.wl_display, 6)/' \
    "$cage_source" > "$output/cage.c.next"
mv -- "$output/cage.c.next" "$cage_source"
[[ $(digest "$cage_source") == "$PATCHED_SHA256" ]] || fail 'patched Cage source does not match the reviewed change'
diff_status=0
diff -u --label a/cage.c --label b/cage.c "$output/cage.c.before" "$cage_source" \
    > "$output/xdg-v6.patch" || diff_status=$?
[[ $diff_status == 1 ]] || fail 'could not record the unique source patch'
receipt_update '. + {patch_sha256:$sha}' --arg sha "$(digest "$output/xdg-v6.patch")"

stage=configure-native-build
build="$output/build"
mkdir -- "$build"
run pkg-config --variable=have_xwayland wlroots-0.20
have_xwayland=$command_stdout
case "$have_xwayland" in true) xwayland_define=1;; false) xwayland_define=0;; *) fail 'wlroots did not expose its Xwayland build setting';; esac
sed -e "s/#mesondefine CAGE_HAS_XWAYLAND/#define CAGE_HAS_XWAYLAND $xwayland_define/g" \
    -e 's/#mesondefine CAGE_VERSION/#define CAGE_VERSION "0.3.1"/g' \
    "$source_directory/config.h.in" > "$build/config.h"
if [[ $(<"$build/config.h") == *'#mesondefine'* ]]; then fail 'unexpected upstream configuration substitution'; fi
receipt_update '. + {config_sha256:$sha, have_xwayland:$xwayland}' \
    --arg sha "$(digest "$build/config.h")" --argjson xwayland "$have_xwayland"

sources=(cage.c idle_inhibit_v1.c output.c seat.c view.c xdg_shell.c)
[[ $have_xwayland != true ]] || sources+=(xwayland.c)
source_paths=()
for file in "${sources[@]}"; do source_paths+=("$source_directory/$file"); done
run pkg-config --cflags --libs wlroots-0.20 wayland-server xkbcommon
split_flags "$command_stdout"
binary="$build/cage"
stage=compile-cage
run cc -std=c11 -O2 -DWLR_USE_UNSTABLE -Wundef -Wno-unused-parameter \
    -Wall -Wextra -Werror -I "$build" -I "$source_directory" \
    "${source_paths[@]}" -o "$binary" "${pkg_flags[@]}" -lm
stage=check-cage-version
run "$binary" -v
receipt_update '. + {status:"built", binary:$binary, binary_sha256:$sha, binary_version:$version}' \
    --arg binary "$binary" --arg sha "$(digest "$binary")" --arg version "$command_stdout"
complete=true
stage=complete
