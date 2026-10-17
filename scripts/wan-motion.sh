#!/usr/bin/env bash
# Screen activity for the viewer test bench (docs/WAN-BENCH.md). Run inside a clone with a
# desktop, as the clone user, while that clone is selected:
#
#   scripts/wan-motion.sh still|scroll|large
#
# Each call first stops the activity of the call before it.
#   still   nothing moves.
#   scroll  a 960×540 window scrolls a page of text at 240 px/s.
#   large   the same text scrolls full screen: every frame changes.
# Both are on monitor 1 (x 1920..3840 of the default two-monitor layout): monitor 0 has the
# top bar, and Mutter puts a full-screen window there on monitor 1 anyway.
# Both use ffplay on a picture of text that is made once (GPL-3 in two columns of a monospace font, light on
# dark like an editor) and scroll it at 60 frames per second.
set -euo pipefail

TAG=rmng-wan-bench
IMG=/tmp/$TAG.png
export XDG_RUNTIME_DIR=/run/user/$(id -u) DISPLAY=:0
XAUTHORITY=$(ls "$XDG_RUNTIME_DIR"/.mutter-Xwaylandauth.* | head -1)
export XAUTHORITY

pkill -f "window_title $TAG" || true
[ "${1:-}" = still ] && exit 0

if [ ! -f "$IMG" ]; then
  font=$(fc-match -f '%{file}' monospace)
  # Two columns, so the text fills the width.
  lic=/usr/share/common-licenses/GPL-3
  pr -m -t -w 170 "$lic" <(tail -n +340 "$lic") >/tmp/$TAG.txt
  ffmpeg -loglevel error -f lavfi -i color=c=0x1e1e1e:s=1920x6000 -frames:v 1 \
    -vf "drawtext=fontfile=$font:textfile=/tmp/$TAG.txt:expansion=none:fontsize=18:line_spacing=6:fontcolor=0xd4d4d4:x=24:y=12" \
    "$IMG"
fi

case "${1:-}" in
  scroll) w=960 h=540 extra=(-left 2400 -top 270) ;;
  large) w=1920 h=1080 extra=(-left 1920 -top 0 -fs) ;;
  *) echo "usage: $0 still|scroll|large" >&2; exit 2 ;;
esac
# 4 px per frame at 60 frames per second; the picture is decoded once and looped.
setsid ffplay -hide_banner -loglevel error -window_title $TAG -noborder "${extra[@]}" -f lavfi \
  "movie=$IMG,loop=loop=-1:size=1,setpts=N/(60*TB),crop=$w:$h:0:'mod(n*4,ih-$h)'" \
  </dev/null >/dev/null 2>&1 &
