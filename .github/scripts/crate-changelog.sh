#!/bin/sh
# Adds the crates in [dependencies] that moved since the last release to CHANGELOG.md, with the pull requests that moved them.
set -eu

FILE="${CHANGELOG:-CHANGELOG.md}"
scripts=$(dirname "$0")

last=$(sed -n 's|^\[Unreleased\]: .*/compare/\(.*\)\.\.\.HEAD$|\1|p' "$FILE")
[ -n "$last" ] || exit 0

locked() {
    awk -v want="$1" '
        /^\[\[package\]\]/ { name = ""; root = 0 }
        /^name = / { name = $3; gsub(/"/, "", name); root = (name == "avet") }
        /^version = / && name == want { count++; only = $3; gsub(/"/, "", only) }
        root && /^ "/ {
            dep = $0
            gsub(/^ "|",?$/, "", dep)
            split(dep, part, " ")
            if (part[1] == want && part[2] != "") pinned = part[2]
        }
        END {
            version = pinned != "" ? pinned : (count == 1 ? only : "")
            sub(/\+.*/, "", version)
            print version
        }'
}

pulls() {
    git log --format='%H %s' "$last..HEAD" -- Cargo.lock | while read -r commit subject; do
        pull=$(printf '%s\n' "$subject" | sed -n 's/.* (#\([0-9][0-9]*\))$/\1/p')
        [ -n "$pull" ] || continue
        before=$(git show "$commit^:Cargo.lock" 2>/dev/null | locked "$1")
        after=$(git show "$commit:Cargo.lock" | locked "$1")
        [ "$before" = "$after" ] || printf '%s ' "$pull"
    done
}

stamp() {
    NAME="$1" FROM="$2" TO="$3" PULLS="$4" awk '
        /^## \[/ { unreleased = ($0 ~ /^## \[Unreleased\]/) }
        /^### / { deps = ($0 == "### Dependencies") }
        unreleased && deps && /^- / {
            split($0, part, " ")
            if (part[2] == ENVIRON["NAME"] && part[4] == "->") {
                all = ENVIRON["PULLS"]
                if (match($0, / \(#[0-9, #]+\)$/)) all = all " " substr($0, RSTART + 2, RLENGTH - 3)
                $0 = "- " part[2] " " ENVIRON["FROM"] " -> " ENVIRON["TO"]
                gsub(/[#,]/, " ", all)
                count = split(all, found, " ")
                for (i = 1; i <= count; i++) {
                    for (j = i + 1; j <= count; j++) {
                        if (found[j] + 0 < found[i] + 0) { swap = found[i]; found[i] = found[j]; found[j] = swap }
                    }
                }
                list = ""
                for (i = 1; i <= count; i++) {
                    if (i == 1 || found[i] != found[i - 1]) list = list (list ? ", " : "") "#" found[i]
                }
                if (list != "") $0 = $0 " (" list ")"
            }
        }
        { print }' "$FILE" > "$FILE.tmp"
    mv "$FILE.tmp" "$FILE"
}

for crate in $(awk '/^\[/ { on = ($0 == "[dependencies]"); next } on && /^[A-Za-z0-9_-]+ *=/ { print $1 }' Cargo.toml); do
    from=$(git show "$last:Cargo.lock" 2>/dev/null | locked "$crate")
    to=$(locked "$crate" < Cargo.lock)
    if [ -z "$from" ] || [ -z "$to" ] || [ "$from" = "$to" ]; then continue; fi
    CHANGELOG="$FILE" sh "$scripts/changelog.sh" dependency "$crate" "$from" "$to"
    stamp "$crate" "$from" "$to" "$(pulls "$crate")"
done
