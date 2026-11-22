#!/usr/bin/env bash
# Keep the owned session leader alive after the program exits. The runner sends
# TERM to the entire group, then KILL after its grace period. This stable leader
# keeps the group identity valid while stubborn helper processes are stopped.
set -u
prefix=$1
shift
trap ':' TERM INT HUP

"$@" &
command_pid=$!
command_start=0
if [[ -r /proc/$command_pid/stat ]]; then
    IFS= read -r stat_line < "/proc/$command_pid/stat" || stat_line=
    stat_tail=${stat_line##*) }
    read -r -a stat_fields <<< "$stat_tail"
    if (( ${#stat_fields[@]} >= 20 )); then command_start=${stat_fields[19]}; fi
fi
printf '%s %s\n' "$command_pid" "$command_start" > "$prefix.child"

while :; do
    if wait "$command_pid"; then result=0; else result=$?; fi
    # A trapped TERM can interrupt wait before the program actually exits.
    # Continue waiting only for the same child instance, never a reused PID.
    if [[ -r /proc/$command_pid/stat ]]; then
        IFS= read -r stat_line < "/proc/$command_pid/stat" || stat_line=
        stat_tail=${stat_line##*) }
        read -r -a stat_fields <<< "$stat_tail"
        if (( ${#stat_fields[@]} >= 20 )) && [[ ${stat_fields[19]} == "$command_start" && ${stat_fields[0]} != Z && ${stat_fields[0]} != X ]]; then
            continue
        fi
    fi
    break
done
printf '%s\n' "$result" > "$prefix.status"
while :; do
    sleep 1 &
    wait "$!" || true
done
