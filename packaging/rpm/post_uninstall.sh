#!/bin/sh
# RPM %postun (post-uninstall): refresh desktop/icon caches after removal.
# Files of the package are already gone at this point, so we only refresh the
# shared databases (best-effort, never fail the transaction).
set +e

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications 2>/dev/null
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q /usr/share/icons/hicolor 2>/dev/null
fi

exit 0
