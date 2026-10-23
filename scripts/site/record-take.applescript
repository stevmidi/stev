#!/usr/bin/osascript

# Records one take for the website's Capture table (docs/280-roadmap.md § The
# website). Places the stev window at a fixed origin and size, then records
# that rectangle for 30 s into
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

# The window's frame in points, fixed so every take records the same pixels.
# Smaller than the screenshot's 1900×1050 on purpose: the app draws its UI at
# a fixed size, so a smaller window keeps the text readable in a ~600 px
# table cell. Record with the browser panel closed.
property winX : 100
property winY : 100
property winW : 1100
property winH : 700
# Long enough for the slowest take (stopped, a clip); the encoder trims it.
property recordSeconds : 30

on run argv
	set take to item 1 of argv
	set outDir to POSIX path of (path to home folder) & "Documents/Stev/Recordings"
	set outFile to outDir & "/" & take & ".mov"
	# screencapture's errors (a missing Screen Recording permission shows up
	# as "capture error"), since a background run has nowhere else to say so.
	set logFile to outDir & "/" & take & ".log"

	tell application "System Events" to tell process "stev"
		set frontmost to true
		set position of window 1 to {winX, winY}
		set size of window 1 to {winW, winH}
	end tell

	set rect to (winX as text) & "," & (winY as text) & "," & (winW as text) & "," & (winH as text)
	do shell script "mkdir -p " & quoted form of outDir
	# A retake replaces the take: screencapture won't overwrite a video file.
	do shell script "rm -f " & quoted form of outFile
	# In the background, so Raycast returns and the keyboard goes back to stev.
	do shell script "screencapture -x -v -V" & recordSeconds & " -R" & rect & " " & quoted form of outFile & " > " & quoted form of logFile & " 2>&1 &"
	return "Recording " & take & " for " & recordSeconds & " s"
end run
