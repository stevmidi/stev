#!/usr/bin/osascript

# Records one take for the website's Capture table (docs/280-roadmap.md § The
# website). Places the stev window as screenshot.applescript does, at a fixed
# origin, then records that rectangle for 30 s into
# ~/Documents/Stev/Recordings/<take>.mov, outside the repo. encode-take.sh
# trims and encodes it. Runs as a Raycast script command or from a terminal:
# osascript scripts/site/record-take.applescript stopped-empty

# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Stev Record Take
# @raycast.mode compact

# Optional parameters:
# @raycast.icon 🎬
# @raycast.argument1 { "type": "text", "placeholder": "take name, e.g. stopped-empty" }

# Documentation:
# @raycast.description Places the stev window and records it for 30 s, for the website's capture loops.

# The window's frame in points: screenshot.applescript's size, at a fixed
# origin so every take records the same pixels.
property winX : 100
property winY : 100
property winW : 1900
property winH : 1050
# Long enough for the slowest take (stopped, a clip); the encoder trims it.
property seconds : 30

on run argv
	set take to item 1 of argv
	set outDir to POSIX path of (path to home folder) & "Documents/Stev/Recordings"
	set outFile to outDir & "/" & take & ".mov"

	tell application "System Events" to tell process "stev"
		set frontmost to true
		set position of window 1 to {winX, winY}
		set size of window 1 to {winW, winH}
	end tell

	set rect to (winX as text) & "," & (winY as text) & "," & (winW as text) & "," & (winH as text)
	do shell script "mkdir -p " & quoted form of outDir
	# In the background, so Raycast returns and the keyboard goes back to stev.
	do shell script "screencapture -x -v -V" & seconds & " -R" & rect & " " & quoted form of outFile & " > /dev/null 2>&1 &"
	return "Recording " & take & " for " & seconds & " s"
end run
