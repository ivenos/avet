#!/bin/sh
# Writes the Docker Hub description: the logo and introduction from the top of README.md, and a link to the repository.
set -eu

repo="${1:?usage: dockerhub-description.sh OWNER/REPO REF}"
ref="${2:?usage: dockerhub-description.sh OWNER/REPO REF}"

awk -v raw="https://raw.githubusercontent.com/$repo/$ref/" -v url="https://github.com/$repo" '
    /shieldcn\.dev|<\/?picture>|<source / { next }
    /<img / && logo++ { next }
    /^[[:space:]]*$/ { if (blank++) next; print ""; next }
    {
        blank = 0
        if (match($0, /src="[^"]*"/) && substr($0, RSTART + 5, 4) != "http") $0 = substr($0, 1, RSTART + 4) raw substr($0, RSTART + 5)
        sub(/^[[:space:]]+/, "")
        print
    }
    /^<\/div>/ { exit }
    END { print "\n---\n\nMore information on [GitHub](" url ")." }
' README.md
