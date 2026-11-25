# The Lane's Octave Buttons

**Status: agreed and built 2026-11-25 (as built in `110` § Scope, § MIDI routing and § Rendering).** With the performance lane selected, the keyboard's previous/next buttons (CC28/CC29, on the MicroLab `⇧` + octave down/up) move the player's hand an octave down/up along the lane, at once. The keyboard's own octave stays at home. A cursor move places the hand around the cursor, and `560` § 4's guess goes. Read `560` (the lane's reach and the hand) and `110` first. This takes up `560` § Not in this design's "a CC from the keyboard's octave buttons" bullet.

## Background

Most keyboards, the MicroLab among them, shift their octave internally and send no MIDI message for the octave buttons. Stev can't see the shift, so today:

- the lane learns about it only from the next note played (`560` § 5): the lit hand jumps on that first press, not when the button is pressed;
- the hand is a **guess** (`KeybedWindow::played`) that can be an octave off until a note near the keyboard's edge arrives (`560` § 4).

Weighed and set aside (2026-11-25):

- **An octave per track**, Stev shifting live notes for the track they play. Stored state that changes what a track sounds like, with the keyboard's own buttons stacking on top of it unseen; and it solves a problem not yet met in use.
- **Computer keys** (`Z`/`X`, borrowed from the zoom pair while the lane is selected), **buttons in the lane's gutter**, **dragging the lit hand**. All of them take a hand off the keyboard mid-performance.

The MicroLab sends CC28 (labelled *previous*) and CC29 (*next*) for `⇧` + octave down/up, 127 on press and 0 on release, without changing its octave (checked 2026-11-25). The buttons are meant for stepping through presets in Arturia's plugins; many keyboards have a pair like it. With them the player keeps both hands on the keyboard, and Stev sees every press.

## The design

### 1. The keyboard stays at home

The keyboard's octave isn't shifted. Stev assumes it sends its home octave: the notes `KeybedWindow::new(size)` gives today for the keybed size in settings (49 keys: 36–84, 25 keys: 48–72). The hand is that window, placed along the timeline (§ 2).

### 2. A cursor move places the hand

The hand is the home notes laid unbroken along the timeline from a C, as `560` § 2 lays them; a press jumps to its note's key in the hand (`Hand::key_index`), then the phase offset (`550`) as today.

- **Any cursor move places it** (a click in the lane, `←`/`→`, anything that moves the arranger cursor), and so does selecting the lane: the hand moves, in whole octaves, to where its middle key is nearest the cursor. This is `560`'s rule at an octave's step instead of a 120-key cycle's, so the cursor is always under the hand, within half an octave of its middle. A click in the lane is the "put my hand here" gesture.
- **So does anything that moves the cursor's key** without moving the cursor: the key length (`510` step 1b), the meter, the keybed size.
- **A tie** at exactly half an octave picks the octave ahead, as `560` picks the copy ahead.
- **Never before bar 1**: a placement whose lowest key would fall before bar 1 takes the octave up.
- **The playhead never moves it** (as `560`): a performance keeps its mapping for as long as the cursor stays put.

### 3. The buttons move it by an octave

- **CC28 an octave down, CC29 an octave up**, from where it is, on any value above 0 (the press); the 0 of the release does nothing. Any channel.
- **Only while the lane is selected** (selected is armed, `110` § Arming / selection). Unselected, the two CCs pass as any CC does today, to MIDI thru and the plugin tap: nothing happens on the lane while it isn't armed.
- **Into the lane's handler, in order with the notes.** While armed, `LiveInputRouter::route` sends the two CCs on the `midi_in_tx` channel with the recorded messages (today only notes, the wheels and the pedal go), and `handle_performance_lane_midi_input` takes them on the `"sequencer"` thread, where the lane's presses are handled. A button and then a note arrive in that order, so the note jumps by the moved hand. Neither reaches the capture buffer (armed, the lane handler takes the input instead of `Sequencer::handle_midi_input`) nor anything audible (armed, MIDI thru and the plugin tap are already off).
- **An octave down that would put the lowest key before bar 1** does nothing.
- **The next cursor move places it again** (§ 2).
- **No footer line**: the lit hand moving is the feedback.
- **The numbers are constants** (`config`), 28 and 29. A setting, or a learn ("press your previous button"), when a keyboard that sends others turns up.

### 4. Shown at once

The sequencer owns the hand's placement, as it owns the key length (`510` step 1b): a cursor move and a button press change it there, and the view reads it from `SharedAtomics`, one atomic, no lock (`000`). After a button press the handler wakes the UI (`request_repaint`, `110` § Waking the UI), since a CC isn't a window event, so the lit hand moves as the button is pressed, before any note is played. The press handler reads the same placement it drew from.

- **Not saved.** Selecting the lane places the hand around the cursor, so nothing needs keeping.
- **Not drawn unselected** (as today).

The key names along the lane (`note_for_key`, the 120-key cycle) are unchanged. The hand starts on a C, so a played note's letter matches its key's, though not always its octave number.

### 5. A note outside the home octave

If the keyboard's octave is shifted anyway, its notes still jump (`Hand::key_index`, unbroken past the lit hand's edge), but the lit hand doesn't follow them: an octave press on the keyboard is now a wrong press, as a key outside the hand is. Accepted.

## What goes away

- `KeybedWindow::played` and the guess it moves, `KeybedWindow::resized`, `LiveNotes::lane_press` and the guess's atomic in `LiveNotes`: the input thread no longer moves anything for the lane. The keybed size stays: it sets the hand's width and its home notes.
- `KeybedWindow::hand`'s cycle-step placement, replaced by the octave-step one.
- `560` § 4, and § 5's "after a hardware octave button, only at the next key press".

## Decided

- **A cursor move undoes the buttons' moves** (§ 2, § 3), so a click always puts the hand under the click. The alternative, a lane octave that cursor moves leave alone, was set aside for that reason (2026-11-25).

## What changes in code

- `models/performance_lane.rs`: the hand placed by octave around the cursor, and moved an octave; the guess removed. Tests: the placement either side of the halfway point, the tie, never before bar 1, an octave down refused at bar 1, other keybed sizes, meters and key lengths.
- `core/midi/message.rs`: a pure parse of the two CCs (`lane_octave`: the step, press only). Tests: press and release, other CCs, any channel.
- `core/midi/input.rs`: the two CCs to `midi_in_tx` while armed; the `lane_press` call goes. Test: armed they go to the sequencer, unarmed to thru as before.
- `core/event_handlers/performance_lane_handler.rs`: the CCs move the placement and wake the UI; presses jump by it.
- `core/sequencer`: the placement, written on a cursor move, the lane's selection, a key length, meter or keybed size change; published in `SharedAtomics`. Test: a cursor move after a button places it afresh.
- `core/shared_atomics.rs`: the placement's atomic; `LiveNotes` loses the guess.
- `view/display/rendering/performance_lane.rs`: the lit hand from the placement.
- `config.rs`: the two CC numbers.
- Docs: `110` (§ MIDI routing, § Rendering), `560` (§ 4, § 5 and its Not in this design), `090` if it lists what goes to the sequencer, `CHANGELOG.md`.

## What to check by hand

- The MicroLab at home, the lane selected: `⇧` + octave up moves the lit hand an octave up before any note is played, and a press lands where it's lit; `⇧` + octave down back.
- Button then a note straight after: the note lands in the moved hand.
- A click in the lane puts the hand under the click; the buttons then move it; the next click puts it back under the cursor.
- At bar 1, down does nothing.
- The lane not selected: the buttons do nothing in Stev, and an Arturia plugin on the track still gets them.
- `/` after a performance using the buttons: the capture has no CC28/CC29 in it.

## Changes while building

- **How a placement is triggered**: the cursor is written in a few places (`Transport` on the sequencer thread, `Sequencer::set_cursor_tick`), so the hand is placed from each: `Sequencer::set_cursor_tick`, the transport handler's `MoveCursor` and `SetCursorAndSelectClip` (a click on the cursor's own spot places it too, as a click is the "put my hand here" gesture), `set_meter`, `set_performance_lane_key_length`, arming, and the keybed size, which now goes to the sequencer (`SequencerCommand::SetKeybedSize`) instead of being set on `LiveNotes` by the view.
- **A note whose key would be before bar 1** (the keyboard's octave shifted well down, past § 5's accepted wrong press) does nothing.
- **A held key outside the home notes** lights nothing in the lane (it did, on its mapped key, with the guess).
- **A button arriving after the lane was left** (sent while armed, the lane left before the sequencer took it) is dropped by the sequencer pump, never recorded.

## Phases

One phase, built 2026-11-25.
