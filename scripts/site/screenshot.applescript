#!/usr/bin/osascript

# Takes the arranger screenshot used by the README and the website. Sizes the
# stev window to 1900×1050, then captures it (click the window) into
# docs/images/arranger.png. Take it in the Magenta Pulse theme, which the
# site's palette matches, and keep the <img> width/height in site/index.html
# in step with the size. Runs as a Raycast script command or from a terminal.

# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Stev Screenshot Arranger
# @raycast.mode compact

# Optional parameters:
# @raycast.icon 🤖

# Documentation:
# @raycast.description Sizes the stev window and captures it into docs/images/arranger.png.

set scriptDir to do shell script "dirname " & quoted form of POSIX path of (path to me)
set repo to do shell script "cd " & quoted form of scriptDir & "/../.. && pwd"

tell application "System Events" to tell process "stev" to set size of window 1 to {1900, 1050}
do shell script "screencapture -o -w -x " & quoted form of (repo & "/docs/images/arranger.png")
