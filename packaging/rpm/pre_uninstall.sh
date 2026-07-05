#!/bin/sh
# RPM %preun (pre-uninstall): deregister from MIME defaults BEFORE files are
# removed. The first argument is the install count: "1" means "the package is
# being removed", "2+" means "upgrading". On upgrade we keep the registration
# so %post of the new version can re-add it seamlessly.
set -e

if [ "$1" = "0" ]; then
    HELPER="/usr/lib/app-image-manager/mime-register.sh"
    if [ -x "$HELPER" ]; then
        "$HELPER" remove || {
            echo "app-image-manager: MIME deregistration failed, continuing" >&2
        }
    fi
fi

exit 0
