# Keys and the mouse

Every key STAR/CORD knows, in the order the `?` overlay prints them. This file
is generated from the table in `src/ui/keymap.rs`, and a test fails if the two
disagree.

A key is offered to the focused panel first and to the global table second, so
a binding under a panel heading works while that panel has focus and the global
ones work from everywhere. The composer is the exception: while it has focus it
takes raw keys, because `d` in a sentence is a letter. Every `alt+…`
falls through it, which is what keeps the panel keys working mid-word, and no
plain letter is needed to leave it, so nothing you type can strand you.

## navigation

_everywhere_

| key | what it does |
|---|---|
| `tab`          | next panel |
| `shift+tab`    | previous panel |
| `alt+1`        | focus servers |
| `alt+2`        | focus channels |
| `alt+3`        | focus messages |
| `alt+4`        | focus chat |
| `alt+5 / i`    | write a message |
| `alt+6`        | focus members |
| `up / k`       | up one |
| `down / j`     | down one |
| `shift+up/K`   | up ten |
| `shift+down/J` | down ten |
| `pgup`         | page up |
| `pgdn`         | page down |
| `home / gg`    | to the top |
| `end / G`      | to the bottom |
| `enter`        | open |
| `esc`          | back, or mark read |
| `[`            | previous server |
| `]`            | next server |
| `alt+up`       | previous unread |
| `alt+down`     | next unread |
| `ctrl+k`       | jump to anything |
| `ctrl+f / /`   | search |

## lists

_in the server rail, the channel list, the message list, the member list_

| key | what it does |
|---|---|
| `h / l`        | fold, unfold |

## chat

_in the conversation_

| key | what it does |
|---|---|
| `r`            | reply |
| `R`            | reply without ping |
| `e`            | edit mine |
| `d`            | delete mine |
| `+`            | react |
| `y`            | copy the text |
| `Y`            | copy the link |
| `ctrl+y`       | copy a jump link |
| `o`            | open elsewhere |
| `enter`        | view attachment |
| `space`        | reveal a spoiler |
| `u`            | go to the quoted |
| `p`            | pin, unpin |
| `m`            | mark read |
| `ctrl+u`       | load older |
| `ctrl+e`       | to the newest |

## composer

_in the composer_

| key | what it does |
|---|---|
| `enter`        | send |
| `shift+enter`  | new line |
| `alt+enter`    | new line as well |
| `up`           | edit the last |
| `ctrl+u`       | clear it |
| `ctrl+e`       | emoji |
| `ctrl+g`       | a GIF |
| `alt+a`        | attach a file |
| `ctrl+v`       | paste a picture |
| `esc`          | cancel |

## pickers

_in a picker_

| key | what it does |
|---|---|
| `ctrl+g`       | emoji to GIFs |
| `esc`          | close |

## media viewer

_in the media viewer_

| key | what it does |
|---|---|
| `h`            | previous |
| `l`            | next |
| `s`            | save it |
| `z`            | fit, actual size |

## panels

_everywhere_

| key | what it does |
|---|---|
| `alt+g`        | servers |
| `alt+c`        | channels |
| `alt+d`        | direct messages |
| `alt+m`        | members |
| `alt+z`        | chat only |
| `alt+s`        | panel settings |
| `alt+x`        | close this panel |

## appearance

_everywhere_

| key | what it does |
|---|---|
| `t`            | next theme |
| `T`            | previous theme |
| `alt+t`        | timestamps |
| `alt+v`        | avatars |
| `alt+n`        | animate GIFs |

## application

_everywhere_

| key | what it does |
|---|---|
| `? / F1`       | this list |
| `ctrl+r`       | reconnect now |
| `q / ctrl+c`   | quit |

## the mouse

| where | gesture | what it does |
|---|---|---|
| chat     | wheel                 | scroll three rows |
| chat     | click                 | select a message |
| chat     | double-click          | open the attachment |
| chat     | click a reaction      | add or remove yours |
| chat     | click a link          | open it |
| chat     | click the ↩ line      | go to the quoted |
| chat     | click ↓ n new         | jump to the newest |
| lists    | click, wheel          | choose one |
| lists    | double-click          | open a channel |
| lists    | click a category      | fold or unfold it |
| composer | click                 | place the caret |
| composer | click a chip's ×      | drop the attachment |
| panels   | drag a seam           | resize two panels |
| panels   | click a header word   | what the word says |
| status   | click ? help          | open this list |
| status   | click the channel     | jump to anything |
| status   | click the state       | reconnect now |
