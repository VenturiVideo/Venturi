#!/usr/bin/env bash

write_desktop_entry() {
    # Desktop Exec escaping differs from shell escaping.
    local exec_path="$2" line
    exec_path="${exec_path//\\/\\\\\\\\}"
    exec_path="${exec_path//\"/\\\\\"}"
    exec_path="${exec_path//\$/\\\\\$}"
    exec_path="${exec_path//\`/\\\\\`}"
    exec_path="${exec_path//%/%%}"
    while IFS= read -r line; do
        case "$line" in
            Exec=*) printf 'Exec="%s" %%f\n' "$exec_path" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done < "$1"
}
