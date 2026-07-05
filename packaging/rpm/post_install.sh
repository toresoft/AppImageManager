#!/bin/sh
# RPM %post (post-install): register AppImage Manager as default handler for
# AppImage MIME types. Runs after files are placed, so the helper is present.
set -e

HELPER="/usr/lib/app-image-manager/mime-register.sh"
if [ -x "$HELPER" ]; then
    "$HELPER" add || {
        echo "app-image-manager: MIME registration failed, continuing" >&2
    }
fi

exit 0
