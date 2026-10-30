# Okteta structure definitions

`jcake-db/` describes the on-disk format of database files for Okteta's
Structures tool.

## Install

```bash
mkdir -p ~/.local/share/okteta/structures
```

```bash
cp -r okteta/jcake-db ~/.local/share/okteta/structures/
```

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
