#!/bin/sh
# Copies the license files of every crate avet links into DEST/<crate>-<version>/.
set -eu

dest="$1"
target=$(rustc -vV | sed -n 's/^host: //p')

crates=$(cargo metadata --format-version 1 --locked --filter-platform "$target" | jq -r '
  (.resolve.nodes | map({key: .id, value: [.deps[] | select(any(.dep_kinds[]; .kind == null)) | .pkg]}) | from_entries) as $deps
  | def closure: . as $ids | ($ids + [$ids[] | $deps[.][]] | unique) | if length == ($ids | length) then . else closure end;
  .resolve.root as $root
  | ([$root] | closure) as $linked
  | .packages[]
  | select(.id != $root and (.id | IN($linked[])))
  | "\(.name)-\(.version) \(.manifest_path | rtrimstr("/Cargo.toml"))"')

printf '%s\n' "$crates" | while read -r crate dir; do
    mkdir -p "$dest/$crate"
    find "$dir" -maxdepth 1 -type f \( -iname 'licen[cs]e*' -o -iname 'copying*' -o -iname 'notice*' -o -iname 'unlicense*' \) \
        -exec cp {} "$dest/$crate/" \;
    # REUSE keeps one text per SPDX identifier in LICENSES/.
    [ ! -d "$dir/LICENSES" ] || cp "$dir"/LICENSES/* "$dest/$crate/"
    if [ -z "$(ls -A "$dest/$crate")" ]; then
        echo "error: $crate ships no license file" >&2
        exit 1
    fi
done
