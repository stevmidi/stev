#!/bin/zsh
# Trims a take recorded by record-take.applescript and encodes it for the
# website: H.264 MP4, no audio, yuv420p, +faststart, plus its last frame as the
# poster. Writes site/videos/capture-<take>.mp4 and capture-<take>.jpg.
#
#   scripts/site/encode-take.sh <take> <start> <end> [crop]
#
#   take   name given to the recorder, e.g. stopped-empty
#   start  where the loop starts in the .mov, seconds or hh:mm:ss.ms
#   end    where it ends; the four site loops run 7–19 s
#   crop   optional ffmpeg crop w:h:x:y in recorded pixels. Defaults to the
#          1100×700 window without its 28 px title bar; all four takes use
#          the same crop, so they look alike.
#
# The encoded width is the recorded (or cropped) width, which is already
# about twice a ~600 CSS px table cell, so it stays sharp on a 2x screen.
# WIDTH=… scales it and CRF=… sets the quality if a take comes out over ~1 MB.
# The default 18 keeps the 1 px playhead steady: at 26 its brightness jumped
# from frame to frame in the encoded video.
# Needs ffmpeg (brew install ffmpeg).
set -euo pipefail

if (( $# < 3 )); then
  sed -n '6,17p' "$0"
  exit 1
fi

take=$1 start=$2 end=$3 crop=${4:-1100:672:0:28}
repo=${0:A:h:h:h}
src=~/Documents/Stev/Recordings/$take.mov
out_dir=$repo/site/videos
out=$out_dir/capture-$take.mp4
poster=$out_dir/capture-$take.jpg
width=${WIDTH:-}
crf=${CRF:-18}

filters="fps=30,crop=$crop"
[[ -n $width ]] && filters+=",scale=$width:-2:flags=lanczos"

mkdir -p $out_dir
ffmpeg -hide_banner -loglevel warning -y -ss $start -to $end -i $src \
  -vf $filters -an -c:v libx264 -preset slow -crf $crf -pix_fmt yuv420p \
  -movflags +faststart $out
# The last frame, so the result shows before the loop plays.
ffmpeg -hide_banner -loglevel warning -y -sseof -0.5 -i $out \
  -update 1 -q:v 3 $poster

ls -lh $out $poster | awk '{print $5, $NF}'
ffprobe -v error -select_streams v:0 -show_entries stream=width,height,duration \
  -of csv=p=0 $out
