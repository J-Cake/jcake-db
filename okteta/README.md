# Okteta structure definitions

Two structure definitions for Okteta's Structures tool describe the on-disk
format of database files:

- `jcake-db/` ("jcake-db (fsdb)"): the individual structures (superblock,
  page header, object record, page trailer, summary entry), to apply at the
  cursor. It also holds `common.js`, the helpers both definitions use: CRC-32C,
  the validators, and the JSONB decoder.
- `jcake-db-walk/` ("jcake-db: walk file"): one structure locked to offset 0
  that walks the whole file. It goes from the superblock to each page, then
  through every object on it, then to the page's trailer and summary and on to
  the next page. It needs `jcake-db/` installed alongside it.

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
`okteta/jcake-db`, then `okteta/jcake-db-walk`. It copies them into the
Flatpak data directory above.

### Enable

Restart Okteta, then go to Tools → Structures → Settings → Structures
management and tick "jcake-db (fsdb)" and "jcake-db: walk file".

## Use

### Walking a file

"jcake-db: walk file" is locked to offset 0. Expand `page_offset` to reach
page 0. Each page shows:

- `header`: the page header.
- `objects`: the first object. Each object has a `header`, a `payload` and a
  `next` link to the object after it. For Structured objects, the `payload`
  row shows the decoded JSON.
- `trailer`: the trailer at the end of the page and, if `num_items` isn't 0,
  a `summary` link to the summary table.
- `next_page`: the page after this one.

Rows typed `link` are computed pointers. Okteta can only follow a pointer
stored in the file, so each link overlays the first byte of its parent, and
the number it would show means nothing. The row shows the computed target
instead. A link shows `(end)`, or isn't followed at all, when its target would
be past the end of the file or past the page's trailer. It also stops at
zeroed free space (shown as `(free space)`) and at a header with an impossible
`kind`. A corrupt `length` only makes that object's payload unreadable: its
header still shows, with the CRC error.

The walk is pre-built to `MAX_PAGES` pages and `MAX_OBJECTS_PER_PAGE` objects
per page. Both are set at the top of `jcake-db-walk/main.js`.

### Individual structures

From "jcake-db (fsdb)":

- **Superblock** is locked to offset 0. Expand `page_offset` to follow the
  pointer to page 0's header.
- **Page header**: 28 bytes.
- **Object record**: put the cursor on an object's UUID. The header layout is
  detected from the byte after the 24-bit length:
  - non-zero: the 24-byte layout `ObjectHeader::read_at_position` parses
    (uuid, u24 length, kind, CRC).
  - zero: the 20-byte layout `ObjectHeader::write_struct` currently writes
    (uuid, u32 length). This is flagged as invalid.

  Structured payloads are decoded from Databend JSONB.
- **Page trailer**: the 12-byte `foot` block. Its CRC also covers the summary,
  so it's only checked here when `num_items` is 0.
- **Page summary entry**: one 20-byte `(uuid, offset)` record in the `smry`
  table.

### Validation

Run **Validate** to check magic numbers and CRC-32C checksums. A field that
fails turns red, and its tooltip gives the reason, for example
`page header CRC: stored 0x…, computed 0x…`.

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
node okteta/tools/dump.js fs.db 'Object record' 0x1001c
```

The first command walks the whole file. The second applies one structure from
`main.osd` at the given offset. Lines after `---` are script errors, which
Okteta would put in its structures log. The exit status is non-zero if there
were any, so you can use it to check edits to the definitions.
