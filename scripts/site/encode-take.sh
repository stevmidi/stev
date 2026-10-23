#!/bin/zsh
# Trims a take recorded by record-take.applescript and encodes it for the
# website: H.264 MP4, no audio, yuv420p, +faststart, plus its last frame as the
# poster. Writes site/videos/capture-<take>.mp4 and capture-<take>.webp.
#
#   scripts/site/encode-take.sh <take> <start> <end> [crop]
#
#   take   name given to the recorder, e.g. stopped-empty
#   start  where the loop starts in the .mov, seconds or hh:mm:ss.ms
#   end    where it ends (6–12 s after start)
#   crop   optional ffmpeg crop w:h:x:y in recorded pixels; the same crop for
#          all four takes, so they look alike. Omit to keep the whole window.
#
# WIDTH is the encoded width: a table cell is ~500–600 CSS px, so 1200 stays
# sharp on a 2x screen. WIDTH=… or CRF=… override it and the quality if a
# take comes out over ~1 MB. Needs ffmpeg (brew install ffmpeg).
set -euo pipefail

if (( $# < 3 )); then
  sed -n '6,16p' "$0"
  exit 1
fi

take=$1 start=$2 end=$3 crop=${4:-}
repo=${0:A:h:h:h}
src=~/Documents/Stev/Recordings/$take.mov
out_dir=$repo/site/videos
out=$out_dir/capture-$take.mp4
poster=$out_dir/capture-$take.webp
width=${WIDTH:-1200}
crf=${CRF:-26}

filters="fps=30"
[[ -n $crop ]] && filters+=",crop=$crop"
filters+=",scale=$width:-2:flags=lanczos"

mkdir -p $out_dir
ffmpeg -hide_banner -loglevel warning -y -ss $start -to $end -i $src \
  -vf $filters -an -c:v libx264 -preset slow -crf $crf -pix_fmt yuv420p \
  -movflags +faststart $out
# The last frame, so the result shows before the loop plays.
ffmpeg -hide_banner -loglevel warning -y -sseof -0.5 -i $out \
  -update 1 -c:v libwebp -quality 90 $poster

ls -lh $out $poster | awk '{print $5, $NF}'
ffprobe -v error -select_streams v:0 -show_entries stream=width,height,duration \
  -of csv=p=0 $out
