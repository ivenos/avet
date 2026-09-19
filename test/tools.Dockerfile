FROM alpine:3.24

# dovi_tool and hdr10plus_tool are not in the avet image, and apk needs root.
RUN apk add --no-cache dovi-tool hdr10plus-tool mkvtoolnix
