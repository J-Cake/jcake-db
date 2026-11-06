#!/usr/bin/env node
// Prints a jcake-db file the way Okteta's Structures tool would show it,
// using the definitions in ../jcake-db and ../jcake-db-walk.
//
//   node okteta/tools/dump.js fs.db                        walk the whole file
//   node okteta/tools/dump.js fs.db 'Object record' 0x1001c one main.osd structure
//
// Invalid fields are marked with ✗ and the reason. Lines after "---" are
// script errors and engine messages, which would show up in Okteta's
// structures log.
'use strict';
const path = require('path');
const emu = require('./emu');

const [file, name, offset] = process.argv.slice(2);
if (!file) {
  console.error('usage: dump.js <file.db> [<main.osd structure name> <offset>]');
  process.exit(2);
}
const dir = path.join(__dirname, '..');
const structures = name
  ? emu.loadOsd(path.join(dir, 'jcake-db', 'main.osd'), dir)
  : emu.loadJs(path.join(dir, 'jcake-db-walk', 'main.js'), dir);
const s = name ? structures.find(s => s.name === name) : structures[0];
if (!s) {
  console.error(`no structure named "${name}"; main.osd has: ${structures.map(s => s.name).join(', ')}`);
  process.exit(2);
}
const pluginProblems = ['jcake-db', 'jcake-db-walk'].flatMap(p => emu.checkPlugin(path.join(dir, p)));
const r = emu.run(s, file, Number(offset !== undefined ? offset : s.lock || 0));
if (r.lines.length) console.log(r.lines.join('\n'));
const errors = [...pluginProblems, ...new Set(r.errors)];
console.log(`--- ${r.ok ? 'read' : 'could not read'} structure; ${errors.length} script/engine messages`);
for (const e of errors) console.log('  ' + e);
process.exit(r.ok && !errors.length ? 0 : 1);
