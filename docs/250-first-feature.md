# Your First Feature

A walkthrough of how a key press becomes an undoable edit, for anyone adding a feature: a contributor, someone working on a fork, or a coding agent. It follows one real feature through the code, `M` muting the selected notes in the clip view, and then turns it into a checklist for yours. Every file and function named here exists; open them as you read.

Read `AGENTS.md`'s ground rules first, and `000-architecture.md` § Threading Model if you can. The one fact you need from it: **the UI thread never touches the sequencer's data.** It sends commands over a channel, and the `"sequencer"` thread owns every track, clip and note, applies the change, and sends the UI back what changed.

## With a coding agent

Stev is built with coding agents, and the repo is set up for them. Claude Code loads `AGENTS.md` through `CLAUDE.md` on its own, and most other agents read `AGENTS.md` by convention, so the ground rules (imports, tests in the same change, docs in sync, the build gate) reach the agent without you asking. What it doesn't know is the shape of your feature, and that's what this file gives it.

- **Point it here.** Describe the feature in user terms and name this file:

  > Add a key in the clip view that inverts the velocities of the selected notes, undoable, as one step. Suggest a free key first. Follow `docs/250-first-feature.md` and its checklist.

  Asking for the key first matters: the agent checks `010` and comes back before writing anything, so you choose the binding instead of finding it in the diff.
- **Review against the checklist** at the end of this file. Where an agent is most likely to slip: the key is free in one view but taken in the other; the rule is written inside the edit instead of on `Clip`; the tests cover `edit()` but not undo and redo; `010` changed and the `HELP` table didn't. Ask for the gate's output too: zero warnings, not just a build.
- **Then try it yourself.** `AGENTS.md` tells agents not to launch the app, so playing the feature is always your part. If something's off, describe what you did and what happened, and ask for the fix with a regression test.
- **Before merging**, ask for the `/simplify` pass (`080` § Agentic Editing), then the gate once more.

The rest of this file is the same path read by hand. It's worth skimming even when an agent writes the code: it's how you'll know whether the diff is shaped right.

## The path

```
                                                     ── "main" (UI) thread ──
key press (egui)
  → InputPoller           → InputEvent::KeyPressed { key: M, .. }   view/input_poller.rs
  → Display::forward_input_event   (intercepts only keys that need   view/display/input/mod.rs
                                    view-local state; M in the clip
                                    view passes through)
  → EventHandlers::handle_input_event → handle_mute                  core/event_handlers/input_handler.rs
  → send SequencerCommand::MuteSelectedEvents  ──── channel ────┐    core/sequencer/commands.rs
                                                     ── "sequencer" thread ──
  → handle_sequencer_command → record_edit(MuteSelectedEventsEdit::from_sequencer(..))
                                                                     core/event_handlers/sequencer_handler.rs
  → Record::edit → SequencerEdit::MuteSelectedEvents → edit()        core/sequencer/edit/
  → Clip::toggle_muted_for_selected_events                           models/clip/edits.rs
  → EditResult::EventsModified → handle_edit_result → UiEvents ─ channel ─→ Display repaints
                                                                     core/event_handlers/edit_result_handler.rs
```

⌘Z later runs the same record backwards: `SequencerCommand::Undo` → `Record::undo` → the edit's `undo()` → another `EditResult` through the same `handle_edit_result`. Nothing else is needed for undo to work.

## Step by step

### 1. The key: `InputEvent` → `SequencerCommand`

egui's key presses arrive as `InputEvent::KeyPressed { key, modifiers }` (`core/input_event.rs`). Most of them travel unchanged to `EventHandlers::handle_input_event`, one big `match` in `input_handler.rs`:

```rust
InputEvent::KeyPressed {
    key: Key::M,
    modifiers: _,
} => self.handle_mute(),
```

`handle_mute` decides whether the key means anything *here*. The clip view's `M` only acts with notes selected, so it checks the `ClipContext` (the view plus whether notes are selected) and sends one command:

```rust
fn handle_mute(&self) {
    if matches!(self.clip_context(), ClipContext::ClipWithSelection) {
        self.send_sequencer(SequencerCommand::MuteSelectedEvents);
    }
}
```

The handler only reads atomics (the view, whether there's a selection) and sends. It never reads or changes clips: those live on the other thread.

**Keys that need view state.** A few bindings need something only `Display` knows, such as the arranger's marquee rectangle. Those are intercepted earlier, in `Display::forward_input_event` (its doc comment states the rule), and sent on as a purpose-built `InputEvent` carrying that state. The arranger's `M` is one: it mutes the marqueed range, so `Display` resolves it. If your key doesn't need view-local state, don't add an arm there.

**The command** is a variant of `SequencerCommand` (`core/sequencer/commands.rs`), with a doc comment saying what it does, whether it's undoable, and where it's bound:

```rust
/// Toggles mute on every selected event (its `NoteOn` and paired
/// `NoteOff` together): mute all if any is unmuted, otherwise unmute all
/// ... (`MuteSelectedEventsEdit`, undoable). Bound to `M` in
/// `Clip` with notes selected; ...
MuteSelectedEvents,
```

Carry the operands the sequencer can't work out itself as fields (`TransposeSelectedEvents(i32)` carries the semitones). Anything the sequencer already knows, like the selection, stays out.

### 2. The command: `handle_sequencer_command` → `record_edit`

On the `"sequencer"` thread, `handle_sequencer_command` (`sequencer_handler.rs`) matches the command. For an undoable change, the arm is one call:

```rust
SequencerCommand::MuteSelectedEvents => {
    self.record_edit(
        sequencer,
        undo_record,
        MuteSelectedEventsEdit::from_sequencer(sequencer),
    );
}
```

`record_edit` (`command_helpers.rs`) takes the constructor's `Option`. `None` means there's nothing to do (here: no notes selected), and nothing enters the undo history, so ⌘Z never undoes a step that changed nothing. `Some` is recorded and applied, and its result goes to `handle_edit_result`. It returns whether anything was recorded, if you need to follow up (`TransposeSelectedEvents` uses that to play the new pitch).

**Never change sequencer data directly in a command arm** if the user would expect ⌘Z to undo it. Things that aren't data changes don't go through the record at all: moving the cursor, changing the selection, copying to a clipboard.

### 3. The edit: a `SequencerEdit`

An edit is a struct with a constructor that freezes what it needs, `edit()`, and `undo()`. Which kind to write:

- **It changes the selected notes in one clip.** Use the `clip_events_edit!` macro in `core/sequencer/edit/event_edits.rs`. It snapshots the clip's events before the change, so `undo()` is free. Mute is one invocation:

  ```rust
  clip_events_edit!(
      MuteSelectedEventsEdit;
      post_selection = keep;
      mutate(self, clip) = clip.toggle_muted_for_selected_events()
  );
  ```

  `post_selection` is `keep` (the same notes stay selected) or `clear` (nothing is, as after a delete). Extra fields go after the name (`TransposeSelectedEventsEdit, semitones: i32;`). The macro's doc comment, and `050-undo-redo.md` § `EditResult` and `SequencerEdit`, cover the two cases it can't express.
- **It changes clips, tracks or the timeline.** Hand-write the struct next to its relatives in `edit/clip_edits/` or `edit/track_edits.rs`. `RenameTrackEdit` is the smallest to copy from. Before writing range logic, check whether an existing edit already does part of it: `050` lists the ones that are built from other edits (Insert Silence splits with `SplitClipsEdit`, Delete Time carves with `DeleteInRangeEdit`).

Then register the variant in the `sequencer_edit_dispatch!` list at the bottom of `edit/mod.rs`:

```rust
MuteSelectedEvents(MuteSelectedEventsEdit),
```

That generates the `SequencerEdit` variant, its `undo::Edit` impl and the `From` conversion `record_edit` relies on.

**Put the real work in the model.** The edit only calls `Clip::toggle_muted_for_selected_events` (`models/clip/edits.rs`). The rule (mute all if any is unmuted, move each `NoteOn`'s paired `NoteOff` with it) lives on `Clip`, where it is plain data with no threads or channels, and so is the easiest thing in the codebase to test.

### 4. The result: `EditResult` → the UI

`edit()` and `undo()` return an `EditResult`, which says what changed so the UI can follow. Event edits return `EventsModified { track_idx, clip_id, selected_event_ids, .. }`, and its arm in `handle_edit_result` (`edit_result_handler.rs`) already does everything they need: it re-sends the clip's notes to the view, restores the selection and republishes it. That's why mute has no UI code of its own.

If your edit changes something no existing variant describes, add one (and its undo counterpart) to `EditResult` in `edit/mod.rs`, and give it an arm in `handle_edit_result`. Keep the arm short, or move it into a workflow function in the file for that concern (`clip_lifecycle.rs`, `clip_range_edits.rs`, `tracks.rs`, …). The view is updated only through `UiEvent`s; the handler never reaches into `Display`.

### 5. The tests

Tests go in the same change as the logic (an `AGENTS.md` ground rule), at the levels the feature touches. Mute has two:

- **The model**, in `models/clip/edits.rs`'s `mod tests`: `toggle_muted_mutes_the_selected_note_and_its_paired_note_off`, `toggle_muted_unmutes_when_every_selected_note_is_already_muted`, `toggle_muted_mutes_all_selected_when_any_one_is_unmuted`, `toggle_muted_leaves_unselected_notes_untouched`, `toggle_muted_noop_when_nothing_selected`. One rule per test, named for the rule, so a failure explains itself.
- **The edit**, in `event_edits.rs`'s `mod tests`: the constructor refuses an empty selection (`mute_selected_events_from_sequencer_returns_none_when_nothing_selected`), `edit()` applies the change and reports the right `EditResult`, `undo()` restores the state before, and a redo after the undo applies it again. Every undoable edit needs that edit, undo, redo set. `sequencer_with_two_notes` builds the fixture.

When the handler side has logic of its own (a follow-up after recording, gestures that merge into one undo step, a selection rule), add a test that runs real commands through the handlers: `test_harness.rs` gives you `harness()` and `run_command`, and `inserted_and_dragged_notes_are_one_undo_step_each` in `sequencer_handler.rs` is a good model. A bug fix comes with a regression test that fails without the fix.

### 6. The docs

The docs are written for the next person or agent to change the code, so they change with it, in the same commit (`AGENTS.md`, "Docs stay in sync"). For a new key and edit:

- **`010-keybindings.md`**: the binding, where it applies, and what it does there.
- **The help overlay**: the `HELP` table in `src/view/display/help_overlay.rs` (`("M", "Mute notes")` in the clip view's section). It's hand-written, so nothing reminds you.
- **`050-undo-redo.md`**: add the new variant to the list of current `SequencerEdit` variants, and any new `EditResult` variant to its section.
- Anything else you changed that a topic file describes: view state (`020`), persistence (`060`), threading (`000`). `AGENTS.md`'s table says which file covers what.

Before taking a new key, check that `010` doesn't already use it, including in the other view.

### 7. Done means the gate is clean

Run the build gate from `AGENTS.md` (`cargo check`, `cargo clippy --all-targets`, `cargo fmt --check`, `cargo test`, `cargo doc --no-deps --document-private-items`) with zero warnings. The clippy run includes `missing_docs_in_private_items`, so every new item, field and variant needs a doc comment. Then `/simplify` over the branch's diff (`080` § Agentic Editing), and try the feature in the app yourself: no test covers how it feels.

## Checklist

1. A free key (`010`), matched in `handle_input_event`, gated on the view and selection it applies to.
2. A `SequencerCommand` variant with a doc comment.
3. An arm in `handle_sequencer_command` calling `record_edit` with the edit's constructor.
4. The edit: `clip_events_edit!` for selected notes, hand-written for anything larger; its constructor returns `None` when there's nothing to do.
5. Registered in `sequencer_edit_dispatch!`.
6. The rule itself on the model (`Clip`, `Track`, …), not in the edit.
7. An existing `EditResult`, or a new pair plus its `handle_edit_result` arm.
8. Tests: the model's rule, and the edit's edit, undo, redo and `None`.
9. Docs: `010`, the `HELP` table, `050`, and any topic file whose subject you changed.
10. The build gate, clean.
