#!/bin/sh
set -e

if command -v update-desktop-database > /dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi

if command -v gtk-update-icon-cache > /dev/null 2>&1; then
    gtk-update-icon-cache -q -f /usr/share/icons/hicolor || true
fi

# Sentinel: the in-app updater suppresses itself when this file is present.
mkdir -p /usr/lib/zerminal
if command -v dpkg-query > /dev/null 2>&1; then
    echo apt > /usr/lib/zerminal/.managed
elif command -v rpm > /dev/null 2>&1; then
    echo dnf > /usr/lib/zerminal/.managed
fi
