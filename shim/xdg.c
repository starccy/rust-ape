// The XDG base directories on hosts with conventions of their own. Code
// compiled for Linux keeps data, configuration and caches under
// $XDG_DATA_HOME, $XDG_CONFIG_HOME and $XDG_CACHE_HOME, or ~/.local/share,
// ~/.config and ~/.cache without them. On Windows and macOS the ones the
// user hasn't set are pointed at where native programs keep the same
// things, which is where the dirs crate, tauri's path API and glib look.
//
// BROWSER is set the same way, to the host's own way of opening a URL.
// The webbrowser crate and Python's webbrowser module try it first, and
// they search PATH for xdg-open themselves, so the fork's stand-in for
// a missing xdg-open never gets to run for them.

#include <stdio.h>
#include <stdlib.h>
#define _COSMO_SOURCE // for libc/dce.h's IsWindows()
#include <libc/dce.h>

static void xdg_set(const char *var, const char *base, const char *tail) {
    char buf[4096];
    // cosmo has turned C:\... into /C/... by now, and anything else
    // would be taken for a relative path and ignored
    if (getenv(var) || !base || *base != '/') return;
    if (snprintf(buf, sizeof(buf), "%s%s", base, tail) >= (int)sizeof(buf)) return;
    setenv(var, buf, 0);
}

__attribute__((__constructor__)) static void xdg_init(void) {
    if (IsWindows()) {
        const char *roaming = getenv("APPDATA");
        const char *local = getenv("LOCALAPPDATA");
        xdg_set("XDG_DATA_HOME", roaming, "");
        xdg_set("XDG_CONFIG_HOME", roaming, "");
        xdg_set("XDG_CACHE_HOME", local, "");
        xdg_set("XDG_STATE_HOME", local, "");
        setenv("BROWSER", "rundll32.exe url.dll,FileProtocolHandler", 0);
    } else if (IsXnu()) {
        const char *home = getenv("HOME");
        xdg_set("XDG_DATA_HOME", home, "/Library/Application Support");
        xdg_set("XDG_CONFIG_HOME", home, "/Library/Application Support");
        xdg_set("XDG_CACHE_HOME", home, "/Library/Caches");
        xdg_set("XDG_STATE_HOME", home, "/Library/Application Support");
        setenv("BROWSER", "open", 0);
    }
}
