# Source this: an isolated HOME/XDG profile and an offscreen Qt, so nothing touches the real install or desktop.
RT_LINUX="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export RT_PROFILE="${RT_PROFILE:-$RT_LINUX/.profile}"
export HOME="$RT_PROFILE/home" XDG_DATA_HOME="$RT_PROFILE/home/.local/share" XDG_CONFIG_HOME="$RT_PROFILE/home/.config"
export XDG_CACHE_HOME="$RT_PROFILE/home/.cache" XDG_STATE_HOME="$RT_PROFILE/home/.local/state"
export QT_QPA_PLATFORM=offscreen DRIVE_DIR="$RT_PROFILE/drv"
# Without these Qt asks the KDE portal for file dialogs, which pop up on the real desktop
unset KDE_FULL_SESSION KDE_SESSION_VERSION XDG_CURRENT_DESKTOP DESKTOP_SESSION QT_QPA_PLATFORMTHEME
mkdir -p "$HOME" "$DRIVE_DIR"
