# Okteta structure definitions

Two structure definitions for Okteta's Structures tool describe the on-disk
format of database files:

- `jcake-db` ("jcake-db (fsdb)"): the individual structures (superblock,
  page header, object record, page trailer, summary entry), to apply at the
  cursor. It also holds `common.js`, the helpers both definitions use: CRC-32C,
  the validators, and the JSONB decoder.
- `jcake-db-walk` ("jcake-db: walk file"): one structure locked to offset 0
  that walks the whole file. It goes from the superblock to each page, then
  through every object on it, then to the page's trailer and summary and on to
  the next page. It needs `jcake-db` installed alongside it.

`tools/dump.js` prints the same view from the command line, with no Okteta
needed (see [Without Okteta](#without-okteta)).

## Install

Okteta looks for structures in `$XDG_DATA_HOME/okteta/structures`. Where that
is depends on how Okteta was installed. Install both folders: the walker loads
`jcake-db/common.js` by that path.

### Native package

```bash
mkdir -p ~/.local/share/okteta/structures
```

```bash
cp -r okteta/jcake-db okteta/jcake-db-walk ~/.local/share/okteta/structures/
```

### Flatpak (`org.kde.okteta`)

Flatpak sets `XDG_DATA_HOME` to `~/.var/app/org.kde.okteta/data`, so the
sandboxed Okteta never looks in `~/.local/share`. Granting that directory in
Flatseal doesn't change this: it lets Okteta read the files there, but Okteta
still doesn't search there.

To install a copy, run from the repository root:

```bash
mkdir -p ~/.var/app/org.kde.okteta/data/okteta/structures
```

```bash
cp -r okteta/jcake-db okteta/jcake-db-walk ~/.var/app/org.kde.okteta/data/okteta/structures/
```

To make edits to the repository show up without copying again, link to the
checkout instead and let the sandbox read it. Run from the repository root:

```bash
flatpak override --user --filesystem="$PWD/okteta:ro" org.kde.okteta
```

```bash
ln -sfn "$PWD/okteta/jcake-db" ~/.var/app/org.kde.okteta/data/okteta/structures/jcake-db
```

```bash
ln -sfn "$PWD/okteta/jcake-db-walk" ~/.var/app/org.kde.okteta/data/okteta/structures/jcake-db-walk
```

The links only resolve inside the sandbox if the checkout's path is exposed,
which is what the `flatpak override` is for. You can also grant it in Flatseal
under Filesystem → Other files.

Recent Okteta versions can also install the folders for you: click
**Install from File(s)…** in the Structures management settings and pick
`jcake-db`, then `jcake-db-walk`. It copies them into the
Flatpak data directory above.

### Enable

Restart Okteta, then go to Tools → Structures → Settings → Structures
management and tick "jcake-db (fsdb)" and "jcake-db: walk file".

## Use

Structures other than the two locked to offset 0 are read at the cursor. With
the cursor at the end of the file (where Okteta puts it after opening or
selecting), they show `<EOF reached>` until you move it.

### Walking a file

"jcake-db: walk file" is locked to offset 0. Expand `page_offset` to reach
page 0. Each page shows:

- `header`: the page header.
- `objects`: the first object, at page offset 0x20. Objects start on 16-byte
  boundaries relative to the page, so each `next` link skips the zero padding
  after the previous payload. Each object has:
  - `header`: the object header.
  - `data`: a link to the raw payload bytes.
  - `json`: a link to the payload decoded as JSONB, whose row shows the JSON
    text. It reads `(not JSONB)` for Raw, Tombstone and Reference objects.
  - `next`: a link to the object after it.
- `trailer`: the trailer at the end of the page and, if `num_items` isn't 0,
  a `summary` link to the summary table. Each summary entry's `offset` is a
  pointer you can expand to the object header it names. Validation checks
  that the header there has the entry's UUID.
- `next_page`: the page after this one.

Rows typed `link` are computed pointers. Okteta can only follow a pointer
stored in the file, so each link overlays the first byte of its parent, and
the number it would show means nothing. The row shows the computed target
instead. A link shows `(end)`, or isn't followed at all, when its target would
be past the end of the file or past the page's trailer. It also stops at
zeroed free space (shown as `(free space)`) and at a header with an impossible
`kind`. A corrupt `length` only makes that object's payload unreadable: its
header still shows, flagged as invalid because the CRC covers the payload.

The walk is pre-built to `MAX_PAGES` pages and `MAX_OBJECTS_PER_PAGE` objects
per page. Both are set at the top of `jcake-db-walk/main.js`.

### Individual structures

From "jcake-db (fsdb)":

- **Superblock** is locked to offset 0. Expand `page_offset` to follow the
  pointer to page 0's header.
- **Page header**: 28 bytes.
- **Object record**: put the cursor on an object's UUID, at a 16-byte
  boundary relative to the page. The first object is at page offset 0x20. Its
  `record` row holds the 24-byte header (uuid, u24 length, kind, CRC) and the
  payload. Structured payloads appear as a `jsonb` row showing the decoded
  JSON; other kinds as raw `data`. The CRC covers the uuid, the length as a
  u32 and the payload, but not the kind. If a corrupt length runs past the end
  of the file, the whole record can't be read; the walker still shows such an
  object's header.
- **Page trailer**: the 12-byte `foot` block. Its CRC covers `foot` and
  `num_items` only.
- **Page summary entry**: one 20-byte `(uuid, offset)` record in the `smry`
  table.

### Validation

Run **Validate** to check magic numbers and CRC-32C checksums. A field that
fails turns red, and its tooltip gives the reason, for example
`page header CRC: stored 0x…, computed 0x…`. If the stored value is the
bitwise NOT of the computed one, the message says so: the writer skipped the
CRC's final inversion.

### JSONB decoding

The decoder in `common.js` follows jsonb 0.5.6's encoding. That covers
objects, arrays, scalars, all integer widths (decoded exactly, including u64
and i64 extremes), floats, decimals, UTF-8 strings and extensions. If the
bytes don't decode, the row shows `<invalid JSONB: …>` with the reason.

## Without Okteta

`tools/dump.js` runs the same definitions through a small emulation of
Okteta's structure engine and prints the result as a tree, with offsets,
values and validation results:

```bash
node okteta/tools/dump.js fs.db
```

```bash
node okteta/tools/dump.js fs.db 'Object record' 0x10020
```

The first command walks the whole file. The second applies one structure from
`main.osd` at the given offset. Lines after `---` are script errors, which
Okteta would put in its structures log. They also include `FATAL` problems in
the definitions themselves: Okteta drops such a structure without showing it
or saying why. The exit status is non-zero if there were any messages, so you
can use it to check edits to the definitions.
