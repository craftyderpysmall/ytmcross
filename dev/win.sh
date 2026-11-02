#!/bin/sh
# Drive the window, in client pixels (no decorations on Hyprland):
#   win.sh shot <out.png> | click <x> <y> | rclick <x> <y> | move <x> <y> | type <text> | key <mod> <key>
# click/rclick need ydotool. Typing needs ytmcross focused and shots need it
# visible, so input and captures never reach another window.
action=$1; shift
info=$(hyprctl clients -j | python3 -Ic "
import json, sys
for c in json.load(sys.stdin):
    if c['class'] == 'ytmcross':
        print(*c['at'], *c['size'], c['workspace']['id'])
        break")
[ -n "$info" ] || { echo "no ytmcross window" >&2; exit 1; }
set -- "$@" $info
# grim grabs the screen: on another workspace that's some other window.
visible() {
    eval "workspace=\${$#}"
    hyprctl monitors -j | python3 -Ic "
import json, sys
sys.exit(0 if any(m['activeWorkspace']['id'] == $workspace for m in json.load(sys.stdin)) else 1)" ||
        { echo "ytmcross's workspace isn't visible: not capturing" >&2; exit 1; }
}
focused() {
    [ "$(hyprctl activewindow -j | python3 -Ic "import json, sys; print(json.load(sys.stdin).get('class'))")" = ytmcross ] ||
        { echo "ytmcross is not focused: not typing" >&2; exit 1; }
}
case $action in
    shot) visible "$@"; grim -g "$2,$3 $4x$5" "$1" ;;
    move) hyprctl -q dispatch "hl.dsp.cursor.move({ x = $(($3 + $1)), y = $(($4 + $2)) })" ;;
    click|rclick)
        hyprctl -q dispatch "hl.dsp.cursor.move({ x = $(($3 + $1)), y = $(($4 + $2)) })"
        sleep 0.1
        [ "$action" = click ] && ydotool click 0xC0 || ydotool click 0xC1 ;;
    type) focused; wtype "$1" ;;
    key) focused; wtype -M "$1" -k "$2" -m "$1" ;;
    *) echo "unknown action $action" >&2; exit 1 ;;
esac
