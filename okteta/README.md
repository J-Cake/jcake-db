# Okteta structure definitions

`jcake-db/` describes the on-disk format of database files for Okteta's
Structures tool.

## Install

Okteta looks for structures in `$XDG_DATA_HOME/okteta/structures`. Where that
is depends on how Okteta was installed.

### Native package

```bash
mkdir -p ~/.local/share/okteta/structures
```

```bash
cp -r okteta/jcake-db ~/.local/share/okteta/structures/
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
cp -r okteta/jcake-db ~/.var/app/org.kde.okteta/data/okteta/structures/
```

To make edits to the repository show up without copying again, link to the
checkout instead and let the sandbox read it. Run from the repository root:

```bash
flatpak override --user --filesystem="$PWD/okteta:ro" org.kde.okteta
```

```bash
ln -sfn "$PWD/okteta/jcake-db" ~/.var/app/org.kde.okteta/data/okteta/structures/jcake-db
```

The link only resolves inside the sandbox if the checkout's path is exposed,
which is what the `flatpak override` is for. You can also grant it in Flatseal
under Filesystem → Other files.

Recent Okteta versions can also install the folder for you: click
**Install from File(s)…** in the Structures management settings and pick
`okteta/jcake-db`. It copies the folder into the Flatpak data directory above.

### Enable

Restart Okteta, then go to Tools → Structures → Settings → Structures
management and tick "jcake-db (fsdb)".

## Use

- **Superblock** is locked to offset 0. Expand `page_offset` to follow the
  pointer to page 0's header.
- The other structures follow the cursor. To pin one to an offset, use the
  lock button in the Structures panel.
  - **Page header (as written)**: 32 bytes. This is what `Page::write_struct` writes.
  - **Page header (as read)**: 28 bytes. This is what `Page::read_struct` expects.
  - **Object record**: put the cursor on an object's UUID. Structured payloads
    are decoded as Databend JSONB: the container header, its JEntries, then the data.
  - **Page trailer**: the 12-byte `foot` block.
  - **Page summary entry**: one 20-byte `(uuid, offset)` record in the `smry` table.
- Run **Validate** to check magic numbers and CRC-32C checksums. Invalid fields
  show up in red.
