#!/usr/bin/env bash
# Screen activity for the viewer test bench (docs/WAN-BENCH.md). Run inside a clone with a
# desktop, as the clone user, while that clone is selected:
#
#   scripts/wan-motion.sh still|type|scroll|large|photo
#
# Each call first stops the activity of the call before it.
#   still   nothing moves.
#   type    a 400×100 window shows a new number 5 times per second, as typing does.
#   scroll  a 960×540 window scrolls a page of text at 240 px/s.
#   large   the same text scrolls full screen: every frame changes.
#   photo   a picture full of fine detail (a Mandelbrot set) pans full screen: about 5–9 Mbit/s
#           at QP 23/25, more than the hotspot link carries.
# All are on monitor 1 (x 1920..3840 of the default two-monitor layout): monitor 0 has the
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

font=$(fc-match -f '%{file}' monospace)
if [ ! -f "$IMG" ]; then
  # Two columns, so the text fills the width.
  lic=/usr/share/common-licenses/GPL-3
  pr -m -t -w 170 "$lic" <(tail -n +340 "$lic") >/tmp/$TAG.txt
  ffmpeg -loglevel error -f lavfi -i color=c=0x1e1e1e:s=1920x6000 -frames:v 1 \
    -vf "drawtext=fontfile=$font:textfile=/tmp/$TAG.txt:expansion=none:fontsize=18:line_spacing=6:fontcolor=0xd4d4d4:x=24:y=12" \
    "$IMG"
fi

PIC=/tmp/$TAG-photo.png
if [ "${1:-}" = photo ] && [ ! -f "$PIC" ]; then
  ffmpeg -loglevel error -f lavfi \
    -i "mandelbrot=s=3840x2160:start_scale=0.02:end_scale=0.02:maxiter=1000" -frames:v 1 "$PIC"
fi

case "${1:-}" in
  type)
    setsid ffplay -hide_banner -loglevel error -window_title $TAG -noborder -left 2400 -top 270 \
      -f lavfi "color=c=0x1e1e1e:s=400x100:r=5,drawtext=fontfile=$font:text=%{n}:fontsize=40:fontcolor=0xd4d4d4:x=20:y=30" \
      </dev/null >/dev/null 2>&1 &
    exit 0 ;;
  scroll) w=960 h=540 extra=(-left 2400 -top 270) ;;
  large) w=1920 h=1080 extra=(-left 1920 -top 0 -fs) ;;
  photo)
    # 4 px right and 2 px down per frame, at 60 frames per second.
    setsid ffplay -hide_banner -loglevel error -window_title $TAG -noborder -left 1920 -top 0 -fs \
      -f lavfi "movie=$PIC,loop=loop=-1:size=1,setpts=N/(60*TB),crop=1920:1080:'mod(n*4,iw-1920)':'mod(n*2,ih-1080)'" \
      </dev/null >/dev/null 2>&1 &
    exit 0 ;;
  *) echo "usage: $0 still|type|scroll|large|photo" >&2; exit 2 ;;
esac
# 4 px per frame at 60 frames per second; the picture is decoded once and looped.
setsid ffplay -hide_banner -loglevel error -window_title $TAG -noborder "${extra[@]}" -f lavfi \
  "movie=$IMG,loop=loop=-1:size=1,setpts=N/(60*TB),crop=$w:$h:0:'mod(n*4,ih-$h)'" \
  </dev/null >/dev/null 2>&1 &
