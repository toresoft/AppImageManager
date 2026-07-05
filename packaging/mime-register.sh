#!/bin/sh
#mime-register.sh
#
# Idempotent MIME-handler registration for AppImage Manager, shared by the
# .deb and .rpm maintainer scripts.
#
# Usage:
#   mime-register.sh add     # register AppImage MIME types + refresh caches
#   mime-register.sh remove  # deregister + refresh caches
#
# Operates on /usr/share/applications/mimeapps.list, the system-wide defaults
# override that xdg-mime / freedesktop.org compliant environments honour. The
# file is created if missing; only our MIME types are touched, so other
# defaults are preserved.
#
# Designed to run as root from package post-install / post-uninstall scripts.
# POSIX sh only (no bash-isms) so it works on a minimal Debian/Fedora base.

set -u

APP_ID="appimage-handler.desktop"
MIME_TYPES="application/vnd.appimage application/x-appimage application/octet-stream"
MIMEAPPS="/usr/share/applications/mimeapps.list"

MARK_BEGIN="# BEGIN app-image-manager (do not edit, managed by package)"
MARK_END="# END app-image-manager"

log() {
    printf 'app-image-manager: %s\n' "$*" >&2
}

have() {
    command -v "$1" >/dev/null 2>&1
}

# Rewrite the [Default Applications] block of mimeapps.list so that it
# contains our associations (add) or no longer contains them (remove),
# leaving every other line untouched.
update_mimeapps() {
    action="$1"

    mkdir -p "$(dirname "$MIMEAPPS")" 2>/dev/null || true
    touch "$MIMEAPPS"

    # Split the existing file into:
    #   header  : everything before our managed block
    #   default : the [Default Applications] section body (if any)
    #   other   : other sections, preserved verbatim
    # Then rebuild it deterministically.
    tmp="$(mktemp)"
    tmp_default="$(mktemp)"
    in_default=no
    skip_managed=no

    # Extract an existing [Default Applications] body (excluding our managed
    # lines) so we can re-emit our additions merged with user/packaged ones.
    {
        while IFS= read -r line || [ -n "$line" ]; do
            case "$line" in
                "$MARK_BEGIN"*)
                    skip_managed=yes
                    continue
                    ;;
                "$MARK_END"*)
                    skip_managed=no
                    continue
                    ;;
            esac
            if [ "$skip_managed" = yes ]; then
                continue
            fi
            if printf '%s\n' "$line" | command grep -q '^\['; then
                if printf '%s\n' "$line" | command grep -qi '^\[Default Applications\]'; then
                    in_default=yes
                else
                    in_default=no
                fi
                continue
            fi
            if [ "$in_default" = yes ]; then
                # Skip our own MIME types so we can re-add them cleanly.
                case "$line" in
                    application/vnd.appimage=*|application/x-appimage=*|application/octet-stream=*) : ;;
                    *) printf '%s\n' "$line" ;;
                esac
            fi
        done < "$MIMEAPPS"
    } > "$tmp_default"

    # Now rebuild the full file: keep any non-section header comments, then
    # sections in a stable order.
    {
        # Preserve leading comments/blank lines before the first section.
        in_any_section=no
        in_default=no
        skip_managed=no
        while IFS= read -r line || [ -n "$line" ]; do
            case "$line" in
                "$MARK_BEGIN"*)
                    skip_managed=yes
                    continue
                    ;;
                "$MARK_END"*)
                    skip_managed=no
                    continue
                    ;;
            esac
            if [ "$skip_managed" = yes ]; then
                continue
            fi
            if printf '%s\n' "$line" | command grep -q '^\['; then
                in_any_section=yes
            fi
            if [ "$in_any_section" = no ]; then
                printf '%s\n' "$line"
            fi
        done < "$MIMEAPPS"

        # [Default Applications] section: preserved entries + our managed block.
        printf '%s\n' "[Default Applications]"
        # Re-emit the user/packaged default entries we collected.
        cat "$tmp_default"
        if [ "$action" = add ]; then
            printf '%s\n' "$MARK_BEGIN"
            for mime in $MIME_TYPES; do
                printf '%s=%s\n' "$mime" "$APP_ID"
            done
            printf '%s\n' "$MARK_END"
        fi
        printf '\n'

        # Re-emit every other section verbatim (without [Default Applications],
        # already handled above, and without old managed blocks).
        emit=no
        skip_managed=no
        while IFS= read -r line || [ -n "$line" ]; do
            case "$line" in
                "$MARK_BEGIN"*)
                    skip_managed=yes
                    continue
                    ;;
                "$MARK_END"*)
                    skip_managed=no
                    continue
                    ;;
            esac
            if [ "$skip_managed" = yes ]; then
                continue
            fi
            if printf '%s\n' "$line" | command grep -q '^\['; then
                if printf '%s\n' "$line" | command grep -qi '^\[Default Applications\]'; then
                    emit=no
                    continue
                fi
                emit=yes
                printf '%s\n' "$line"
                continue
            fi
            if [ "$emit" = yes ]; then
                printf '%s\n' "$line"
            fi
        done < "$MIMEAPPS"
    } > "$tmp"

    # Atomic-ish replace.
    cat "$tmp" > "$MIMEAPPS"
    rm -f "$tmp" "$tmp_default"
}

refresh_caches() {
    if have update-desktop-database; then
        update-desktop-database -q /usr/share/applications 2>/dev/null || true
    fi
    # gtk-update-icon-cache works on theme dirs; best-effort.
    if have gtk-update-icon-cache; then
        gtk-update-icon-cache -q /usr/share/icons/hicolor 2>/dev/null || true
    fi
}

case "${1:-}" in
    add)
        log "registering AppImage MIME handler"
        update_mimeapps add
        refresh_caches
        ;;
    remove)
        log "deregistering AppImage MIME handler"
        update_mimeapps remove
        refresh_caches
        ;;
    *)
        log "unknown action: ${1:-<none>}"
        printf 'usage: %s {add|remove}\n' "$0" >&2
        exit 2
        ;;
esac
