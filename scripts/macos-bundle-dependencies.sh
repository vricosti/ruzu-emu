#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "This script only supports macOS." >&2
    exit 1
fi

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 /path/to/ruzu.app [additional Mach-O roots ...]" >&2
    exit 1
fi

app="$1"
shift
additional_roots=("$@")
executable="$app/Contents/MacOS/ruzu"
frameworks="$app/Contents/Frameworks"

if [[ ! -x "$executable" ]]; then
    echo "Missing app executable: $executable" >&2
    exit 1
fi

mkdir -p "$frameworks"

is_system_dependency() {
    case "$1" in
        /System/Library/*|/usr/lib/*) return 0 ;;
        *) return 1 ;;
    esac
}

contains_path() {
    local candidate="$1"
    shift
    local value
    for value in "$@"; do
        if [[ "$value" == "$candidate" ]]; then
            return 0
        fi
    done
    return 1
}

architectures_contain() {
    local container="$1"
    local required="$2"
    local arch
    for arch in $required; do
        case " $container " in
            *" $arch "*) ;;
            *) return 1 ;;
        esac
    done
}

executable_architectures="$(lipo -archs "$executable")"
queue_files=("$executable")
queue_sources=("$executable")
processed=()
dependency_names=()
dependency_sources=()
copied_destination=""

# Files already placed in Frameworks (MoltenVK and GTK runtime modules) are
# roots too: they can carry dependencies not referenced by the main binary.
while IFS= read -r bundled; do
    queue_files+=("$bundled")
    queue_sources+=("$bundled")
done < <(find "$frameworks" -type f -print | sort)

copy_dependency() {
    local source="$1"
    local basename destination source_architectures
    basename="$(basename "$source")"
    destination="$frameworks/$basename"

    # A copied dylib's install name is rewritten to @rpath/<self>. Resolving
    # that entry while walking its dependencies points back to the destination.
    if [[ "$source" == "$destination" ]]; then
        copied_destination="$destination"
        return
    fi

    if [[ ! -f "$source" ]]; then
        echo "Missing non-system dependency: $source" >&2
        exit 1
    fi

    local known_index
    for ((known_index = 0; known_index < ${#dependency_names[@]}; known_index++)); do
        if [[ "${dependency_names[$known_index]}" != "$basename" ]]; then
            continue
        fi
        if ! cmp -s "$source" "${dependency_sources[$known_index]}"; then
            echo "Conflicting bundled libraries named $basename:" >&2
            echo "  existing source: ${dependency_sources[$known_index]}" >&2
            echo "  requested source: $source" >&2
            exit 1
        fi
        copied_destination="$destination"
        return
    done

    if [[ -e "$destination" ]]; then
        dependency_names+=("$basename")
        dependency_sources+=("$source")
        copied_destination="$destination"
        return
    fi

    source_architectures="$(lipo -archs "$source" 2>/dev/null || true)"
    if [[ -z "$source_architectures" ]] ||
        ! architectures_contain "$source_architectures" "$executable_architectures"; then
        echo "Dependency $source does not contain all executable architectures." >&2
        echo "  executable: $executable_architectures" >&2
        echo "  dependency: ${source_architectures:-not a Mach-O library}" >&2
        exit 1
    fi

    install -m 755 "$source" "$destination"
    dependency_names+=("$basename")
    dependency_sources+=("$source")
    queue_files+=("$destination")
    queue_sources+=("$source")
    copied_destination="$destination"
}

if [[ ${#additional_roots[@]} -gt 0 ]]; then
    for root in "${additional_roots[@]}"; do
        copy_dependency "$root"
    done
fi

resolve_special_dependency() {
    local dependency="$1"
    local owner_source="$2"
    local candidate suffix rpath

    case "$dependency" in
        @loader_path/*)
            suffix="${dependency#@loader_path/}"
            candidate="$(dirname "$owner_source")/$suffix"
            [[ -f "$candidate" ]] && printf '%s\n' "$candidate" && return 0
            ;;
        @executable_path/*)
            suffix="${dependency#@executable_path/}"
            candidate="$app/Contents/MacOS/$suffix"
            [[ -f "$candidate" ]] && printf '%s\n' "$candidate" && return 0
            ;;
        @rpath/*)
            suffix="${dependency#@rpath/}"
            candidate="$frameworks/$suffix"
            [[ -f "$candidate" ]] && printf '%s\n' "$candidate" && return 0

            while IFS= read -r rpath; do
                rpath="${rpath//@loader_path/$(dirname "$owner_source")}"
                rpath="${rpath//@executable_path/$(dirname "$executable")}"
                candidate="$rpath/$suffix"
                [[ -f "$candidate" ]] && printf '%s\n' "$candidate" && return 0
            done < <(
                otool -l "$owner_source" | awk '
                    $1 == "cmd" && $2 == "LC_RPATH" { in_rpath = 1; next }
                    in_rpath && $1 == "path" { print $2; in_rpath = 0 }
                '
            )
            ;;
    esac
    return 1
}

index=0
while [[ $index -lt ${#queue_files[@]} ]]; do
    owner="${queue_files[$index]}"
    owner_source="${queue_sources[$index]}"
    index=$((index + 1))

    if [[ ${#processed[@]} -gt 0 ]] && contains_path "$owner" "${processed[@]}"; then
        continue
    fi
    processed+=("$owner")

    if [[ "$owner" != "$executable" && -n "$(otool -D "$owner" | sed -n '2p')" ]]; then
        install_name_tool -id "@rpath/$(basename "$owner")" "$owner"
    fi

    while IFS= read -r dependency; do
        [[ -z "$dependency" ]] && continue
        is_system_dependency "$dependency" && continue

        source=""
        case "$dependency" in
            /*) source="$dependency" ;;
            @*) source="$(resolve_special_dependency "$dependency" "$owner_source" || true)" ;;
            *)
                echo "Unsupported Mach-O dependency in $owner: $dependency" >&2
                exit 1
                ;;
        esac

        if [[ -z "$source" ]]; then
            echo "Cannot resolve Mach-O dependency in $owner: $dependency" >&2
            exit 1
        fi

        copy_dependency "$source"
        destination="$copied_destination"
        replacement="@rpath/$(basename "$destination")"
        if [[ "$dependency" != "$replacement" ]]; then
            install_name_tool -change "$dependency" "$replacement" "$owner"
        fi
    # Universal binaries repeat an unindented "file (architecture ...)" header
    # for every slice. Only indented lines are Mach-O load commands.
    done < <(otool -L "$owner" | awk '/^[[:space:]]/ { print $1 }')
done

if ! otool -l "$executable" | awk '
    $1 == "cmd" && $2 == "LC_RPATH" { in_rpath = 1; next }
    in_rpath && $1 == "path" && $2 == "@executable_path/../Frameworks" { found = 1 }
    END { exit(found ? 0 : 1) }
'; then
    install_name_tool -add_rpath "@executable_path/../Frameworks" "$executable"
fi

failed=false
while IFS= read -r mach_o; do
    while IFS= read -r dependency; do
        [[ -z "$dependency" ]] && continue
        if is_system_dependency "$dependency"; then
            continue
        fi
        case "$dependency" in
            @rpath/*|@loader_path/*|@executable_path/*) ;;
            *)
                echo "Unbundled dependency in $mach_o: $dependency" >&2
                failed=true
                ;;
        esac
    done < <(otool -L "$mach_o" | awk '/^[[:space:]]/ { print $1 }')
done < <(
    find "$app/Contents/MacOS" "$frameworks" -type f -print |
        while IFS= read -r candidate; do
            file "$candidate" | grep -q 'Mach-O' && printf '%s\n' "$candidate"
        done
)

if [[ "$failed" == true ]]; then
    exit 1
fi

echo "Bundled and verified ${#processed[@]} Mach-O files."
