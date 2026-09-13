# Pictures for the replay

Three tiny files the replay hands back when the interface asks for a picture,
and the only binary blobs in the tree.

| File | What it is |
|---|---|
| `avatar.png` | 32x32, a soft disc. Stands in for every avatar and every server icon in `session.json`. |
| `harbour.png` | 64x48, a horizon. The inline image, and the thumbnail on the link card. |
| `cat.gif` | 24x24, three frames, a dot crossing a ground. The animated attachment, and the custom emoji. |

They are **synthetic**, like every other fixture here: no picture was taken
from an account, from the internet, or from anybody's disk. Each is a few
hundred bytes of arithmetic — a radial ramp, a horizon, a moving dot — written
out as PNG and GIF by hand, which is why they are committed rather than
generated at test time. Nothing about them is worth regenerating; if one ever
has to change, any image of the same size and format does.

`testdata/gateway/session.json` maps them to keys under `media`: an avatar or
a server icon by its hash, a custom emoji by its id, and anything carrying a
URL by the last segment of it. See `src/ui/fake.rs`.
