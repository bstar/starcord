# Keys and the mouse

**This page is a stub.** The tables below are generated from
`src/ui/keymap.rs`, which is where every key STAR/CORD answers is written
down, and the generator lands with the rest of the polish milestone along with
the test that fails when this file and that table disagree. Until then the
authoritative list is the `?` overlay, which reads the same table and is
therefore never out of date.

Two rules hold everywhere and are worth knowing before the list exists:

- **A bare arrow moves one and a shifted one moves ten.** True of every list
  and of the message view.
- **`esc` never quits.** It closes an overlay, then cancels an autocomplete,
  then cancels a reply or an edit, then marks the channel read and returns to
  the chat. `q` quits, and asks first if there is a draft.

While the composer has focus it takes raw keys, because it is a text field and
`d` is a letter. Every `alt+…` falls through it, so the panel and appearance
keys work mid-sentence; `tab` and `esc` leave it, and neither is something you
can type by accident.
