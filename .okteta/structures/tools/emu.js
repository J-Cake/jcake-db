// Minimal emulation of Okteta's Structures engine, for the subset of features
// used by okteta/jcake-db (OSD) and okteta/jcake-db-walk (JS), so the
// structure definitions can be exercised without Okteta. See dump.js. Mirrors the
// semantics read from Okteta's source: delayed pointer reads, tagged-union
// selection after reading children, union EOF behaviour, script properties
// (value, uintN, uint64low32/high32, parent, name, length, target, children by
// name/index), ReferenceError on unknown properties, validationError only
// writable while validating, `root` passed as the only argument.
'use strict';
const fs = require('fs'), vm = require('vm'), path = require('path');

const errors = [];
let mode = 'none';

// ---------------------------------------------------------------- descriptors
const PRIM_SIZES = { uint8: 1, uint16: 2, uint32: 4, uint64: 8, char: 1 };
const api = {
  uint8: () => ({ kind: 'prim', type: 'uint8' }),
  uint16: () => ({ kind: 'prim', type: 'uint16' }),
  uint32: () => ({ kind: 'prim', type: 'uint32' }),
  uint64: () => ({ kind: 'prim', type: 'uint64' }),
  char: () => ({ kind: 'prim', type: 'char' }),
  bitfield: (t, width) => ({ kind: 'prim', type: 'bitfield', width }),
  enumeration: (name, type, values) => ({ kind: 'enum', base: type, enumName: name, values }),
  array: (type, length) => ({ kind: 'array', type, length }),
  struct: (fields) => ({ kind: 'struct', fields }),
  union: (fields) => ({ kind: 'union', fields }),
  pointer: (type, target, scale, interpretFunc) => ({ kind: 'pointer', type, target, scale: scale || 1, interpretFunc }),
  taggedUnion: (children, alternatives, defaults) => ({ kind: 'tagged', fields: children, alternatives, defaults: defaults || {} }),
  alternative: (selectIf, fields, name) => ({ selectIf, fields, structName: name }),
};

// ---------------------------------------------------------------- nodes
class Node {
  constructor(desc, name, parent) {
    this.desc = desc; this.name = name; this.parent = parent;
    this.kind = desc.kind; this.read = false; this.addr = -1;
    this.children = [];
    if (desc.kind === 'struct' || desc.kind === 'union' || desc.kind === 'tagged')
      this.children = Object.keys(desc.fields).map(k => new Node(desc.fields[k], k, this));
    if (desc.kind === 'tagged') {
      this.alts = desc.alternatives.map(a => ({ a, nodes: Object.keys(a.fields).map(k => new Node(a.fields[k], k, this)) }));
      this.defaults = Object.keys(desc.defaults).map(k => new Node(desc.defaults[k], k, this));
      this.selected = null;
    }
    if (desc.kind === 'pointer') this.target = new Node(desc.target, '<pointer target>', this);
    if (desc.kind === 'enum') this.prim = desc.base;
  }
  allChildren() {
    if (this.kind === 'tagged') return this.children.concat(this.selected ? this.selected.nodes : this.defaults);
    if (this.kind === 'pointer') return [this.target];
    return this.children;
  }
  byteOrder() {
    for (let n = this; n; n = n.parent) {
      const o = n.desc.byteOrder;
      if (o && o !== 'inherit') return /big/i.test(o) ? 'big' : 'little';
    }
    return 'little';
  }
  root() { let n = this; while (n.parent) n = n.parent; return n; }
}

// ---------------------------------------------------------------- script objects
const proxies = new WeakMap();
function P(node) {
  if (!node) return null;
  if (proxies.has(node)) return proxies.get(node);
  const h = {
    get(_, key) {
      if (typeof key === 'symbol') return undefined;
      const n = node;
      if (key === 'parent') return n.parent ? P(n.parent) : null;
      if (key === 'name') return n.name;
      if (key === 'valid') return n.valid;
      if (key === 'wasAbleToRead') return n.read;
      if (key === 'validationError') return n.validationError;
      if (key === 'toString') return () => '[object ' + n.kind + ' ' + n.name + ']';
      if (key === 'valueOf') return () => NaN;
      if (n.kind === 'prim' || n.kind === 'enum' || n.kind === 'pointer') {
        if (key === 'target' && n.kind === 'pointer') return P(n.target);
        const v = n.rawValue;
        if (['value', 'uint8', 'uint16', 'uint32', 'uint', 'uint64low32', 'uint64high32', 'uint64', 'char'].includes(key)) {
          if (!n.read) { errors.push(`read of unread value ${path_(n)}.${key}`); return undefined; }
          const lo = Number(BigInt.asUintN(32, v)), hi = Number(v >> 32n);
          switch (key) {
            case 'value':
              if (n.type === 'uint64') return v.toString();
              if (n.type === 'char') return String.fromCharCode(Number(v));
              return Number(v);
            case 'uint8': return Number(v & 0xFFn);
            case 'uint16': return Number(v & 0xFFFFn);
            case 'uint32': case 'uint': case 'uint64low32': return lo;
            case 'uint64high32': return hi;
            case 'uint64': return v.toString();
          }
        }
      }
      if (n.kind === 'array') {
        if (key === 'length') return n.children.length;
        if (/^\d+$/.test(key) && +key < n.children.length) return P(n.children[+key]);
      }
      if (n.kind === 'struct' || n.kind === 'union' || n.kind === 'tagged') {
        if (key === 'childCount') return n.allChildren().length;
        const all = n.allChildren();
        if (/^\d+$/.test(key) && +key < all.length) return P(all[+key]);
        const c = all.find(c => c.name === key);
        if (c) return P(c);
      }
      const msg = `ReferenceError: Could not find property with name ${key} on ${path_(node)}`;
      errors.push(msg);
      throw new ReferenceError(msg);
    },
    set(_, key, val) {
      if (key === 'validationError') {
        if (mode !== 'validating') { errors.push(`validationError written outside validation on ${path_(node)}`); return true; }
        node.validationError = val; return true;
      }
      errors.push(`write to ${key} on ${path_(node)} in mode ${mode}`); return true;
    },
  };
  const p = new Proxy({}, h);
  proxies.set(node, p);
  return p;
}
function path_(n) { const parts = []; for (; n; n = n.parent) parts.unshift(n.name); return parts.join('.'); }

function call(fn, node, m) {
  const prev = mode; mode = m;
  try { return fn.call(P(node), P(node.root())); }
  catch (e) { errors.push(`${m} function on ${path_(node)} threw: ${e}`); return undefined; }
  finally { mode = prev; }
}

// ---------------------------------------------------------------- reading
let input, queue;
function readPrim(n, addr) {
  const size = n.type === 'bitfield' ? n.desc.width / 8 : PRIM_SIZES[n.type];
  if (!Number.isInteger(size)) throw new Error('unaligned bitfield not emulated');
  if (addr < 0 || addr + size > input.length) return -1;
  let v = 0n;
  const big = n.byteOrder() === 'big';
  for (let i = 0; i < size; i++) {
    const b = BigInt(input[addr + (big ? i : size - 1 - i)]);
    v = (v << 8n) | b;
  }
  n.rawValue = v; n.read = true;
  return size;
}
function readNode(n, addr) {
  n.addr = addr; n.read = false;
  if (n.desc.updateFunc) call(n.desc.updateFunc, n, 'updating');
  switch (n.kind) {
    case 'prim': n.type = n.desc.type; return readPrim(n, addr);
    case 'enum': n.type = n.desc.base.type; return readPrim(n, addr);
    case 'pointer': {
      n.type = n.desc.type.type;
      const r = readPrim(n, addr);
      if (r < 0) return -1;
      if (interpret(n) < input.length) queue.push(n);
      return r;
    }
    case 'array': {
      let len = n.desc.length;
      if (typeof len === 'function') len = call(len, n, 'length');
      len = Number(len);
      if (!Number.isFinite(len) || len < 0) { errors.push(`bad array length ${len} at ${path_(n)}`); len = 0; }
      n.children = [];
      let off = 0;
      for (let i = 0; i < len; i++) {
        const c = new Node(n.desc.type, String(i), n);
        n.children.push(c);
        const r = readNode(c, addr + off);
        if (r < 0) return -1;
        off += r;
      }
      n.read = true; return off;
    }
    case 'struct': {
      let off = 0;
      for (const c of n.children) { const r = readNode(c, addr + off); if (r < 0) return -1; off += r; }
      n.read = true; return off;
    }
    case 'union': {
      let max = 0, eof = false;
      for (const c of n.children) { const r = readNode(c, addr); if (r < 0) eof = true; else max = Math.max(max, r); }
      n.read = !eof; return eof ? -1 : max;
    }
    case 'tagged': {
      let off = 0;
      for (const c of n.children) { const r = readNode(c, addr + off); if (r < 0) return -1; off += r; }
      n.selected = null;
      for (const alt of n.alts) {
        const s = alt.a.selectIf;
        let ok;
        if (typeof s === 'function') {
          ok = call(s, n, 'selecting');
          if (typeof ok !== 'boolean') { errors.push(`selectIf on ${path_(n)} returned ${ok}`); continue; }
        } else ok = n.children.length === 1 && Number(n.children[0].rawValue) === Number(s);
        if (ok) { n.selected = alt; break; }
      }
      for (const c of n.allChildren().slice(n.children.length)) { const r = readNode(c, addr + off); if (r < 0) return -1; off += r; }
      n.read = true; return off;
    }
  }
  throw new Error('unknown kind ' + n.kind);
}
function interpret(ptr) {
  if (!ptr.desc.interpretFunc) return Number(ptr.rawValue) * ptr.desc.scale;
  const v = call(ptr.desc.interpretFunc, ptr, 'interpreting');
  if (typeof v !== 'number' || !Number.isFinite(v)) { errors.push(`interpretFunc on ${path_(ptr)} returned ${v}`); return 0; }
  return v;
}
function readTop(top, addr) {
  queue = [];
  const r = readNode(top, addr);
  while (queue.length) {
    const p = queue.shift();
    const a = interpret(p);
    if (a < 0 || a >= input.length) { errors.push(`pointer ${path_(p)} does not point to an existing address`); continue; }
    readNode(p.target, a);
  }
  return r;
}

// ---------------------------------------------------------------- validation & dump
function validate(n) {
  for (const c of n.allChildren()) if (c.read || c.kind === 'pointer') validate(c);
  if (n.desc.validationFunc && n.read) {
    const r = call(n.desc.validationFunc, n, 'validating');
    if (typeof r === 'boolean') n.valid = r;
    else if (typeof r === 'string' && r) { n.validationError = r; }
    else if (r !== undefined) errors.push(`validationFunc on ${path_(n)} returned ${r}`);
  }
}
function display(n) {
  if (!n.read) return '<not read>';
  if (n.desc.toStringFunc) { const s = call(n.desc.toStringFunc, n, 'tostring'); if (s !== undefined) return String(s); }
  if (n.kind === 'prim' || n.kind === 'pointer') {
    if (n.type === 'char') return JSON.stringify(String.fromCharCode(Number(n.rawValue)));
    return n.rawValue.toString() + ` (0x${n.rawValue.toString(16)})`;
  }
  if (n.kind === 'enum') {
    const name = Object.keys(n.desc.values).find(k => Number(n.desc.values[k]) === Number(n.rawValue));
    return `${name || '<no enum value>'} (${n.rawValue})`;
  }
  if (n.kind === 'array' && n.children.every(c => c.kind === 'prim')) {
    const vals = n.children.slice(0, 24).map(c => c.type === 'char' ? String.fromCharCode(Number(c.rawValue)) : Number(c.rawValue).toString(16).padStart(2, '0'));
    return (n.children[0] && n.children[0].type === 'char' ? JSON.stringify(vals.join('')) : vals.join(' ')) + (n.children.length > 24 ? ` … (${n.children.length})` : '');
  }
  if (n.kind === 'tagged' && n.selected) return `[${n.selected.a.structName}]`;
  return '';
}
function dump(n, depth = 0, out = [], opts = {}) {
  const status = n.valid === false ? `  ✗ INVALID${n.validationError ? ': ' + n.validationError : ''}` : n.valid === true ? '  ✓' : '';
  const addr = n.addr >= 0 ? '0x' + n.addr.toString(16).padStart(6, '0') : '        ';
  out.push(`${addr} ${'  '.repeat(depth)}${n.name}: ${display(n)}${status}`);
  const skipKids = n.kind === 'array' && n.children.every(c => c.kind === 'prim');
  if (!skipKids && (n.read || n.kind === 'pointer') && depth < (opts.maxDepth || 200))
    for (const c of n.allChildren()) if (c.read || c.kind === 'pointer') dump(c, depth + 1, out, opts);
  return out;
}

// ---------------------------------------------------------------- loaders
function makeContext(structuresDir) {
  const ctx = vm.createContext(Object.assign({}, api));
  ctx.importScript = (rel) => {
    if (rel.includes('..')) throw new Error('path traversal');
    const c = vm.createContext({});
    vm.runInContext(fs.readFileSync(path.join(structuresDir, rel), 'utf8'), c, { filename: rel });
    return c;
  };
  return ctx;
}

function loadJs(file, structuresDir) {
  const ctx = makeContext(structuresDir);
  vm.runInContext(fs.readFileSync(file, 'utf8'), ctx, { filename: file });
  const desc = ctx.init();
  return [{ name: path.basename(path.dirname(file)), desc, lock: desc.defaultLockOffset }];
}

// Just enough XML for .osd files: elements, attributes, comments, the five
// predefined entities.
function parseXml(text) {
  const unescape = v => v.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&apos;/g, "'").replace(/&amp;/g, '&');
  text = text.replace(/<\?[\s\S]*?\?>/g, '').replace(/<!--[\s\S]*?-->/g, '');
  const root = { tag: '#document', attrs: {}, children: [] }, stack = [root];
  const re = /<(\/?)([\w:-]+)((?:\s+[\w:-]+\s*=\s*(?:"[^"]*"|'[^']*'))*)\s*(\/?)>/g;
  let m;
  while ((m = re.exec(text))) {
    if (m[1]) { stack.pop(); continue; }
    const attrs = {};
    for (const a of m[3].matchAll(/([\w:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')/g)) attrs[a[1]] = unescape(a[2] !== undefined ? a[2] : a[3]);
    const el = { tag: m[2], attrs, children: [] };
    stack[stack.length - 1].children.push(el);
    if (!m[4]) stack.push(el);
  }
  return root.children[0];
}

function loadOsd(file, structuresDir) {
  const ctx = makeContext(structuresDir);
  const tree = parseXml(fs.readFileSync(file, 'utf8'));
  const enums = {};
  for (const e of tree.children.filter(e => e.tag === 'enumDef'))
    enums[e.attrs.name] = Object.fromEntries(e.children.map(c => [c.attrs.name, Number(c.attrs.value)]));
  const fn = s => (s && /^\s*function/.test(s)) ? vm.runInContext('(' + s + ')', ctx) : s;
  const common = (d, a) => {
    for (const k of ['validationFunc', 'toStringFunc', 'updateFunc']) if (a[k]) d[k] = fn(a[k]);
    if (a.byteOrder) d.byteOrder = a.byteOrder;
    if (a.typeName) d.typeName = a.typeName;
    return d;
  };
  const prim = t => ({ kind: 'prim', type: t.toLowerCase() });
  const conv = (e) => {
    const a = e.attrs, kids = e.children;
    const fields = list => Object.fromEntries(list.filter(c => !['alternatives', 'defaultFields', 'target', 'type'].includes(c.tag)).map(c => [c.attrs.name || '<anonymous>', conv(c)]));
    switch (e.tag) {
      case 'primitive': return common(prim(a.type), a);
      case 'bitfield': return common({ kind: 'prim', type: 'bitfield', width: Number(a.width) }, a);
      case 'enum': return common({ kind: 'enum', base: prim(a.type), values: enums[a.enum] }, a);
      case 'array': {
        const len = /^\d+$/.test(a.length) ? Number(a.length) : fn(a.length);
        return common({ kind: 'array', type: conv(kids[0]), length: len }, a);
      }
      case 'struct': return common({ kind: 'struct', fields: fields(kids) }, a);
      case 'union': return common({ kind: 'union', fields: fields(kids) }, a);
      case 'pointer': {
        const t = kids.find(c => c.tag === 'target').children[0];
        return common({ kind: 'pointer', type: prim(a.type), target: conv(t), scale: 1 }, a);
      }
      case 'taggedUnion': {
        const alts = (kids.find(c => c.tag === 'alternatives') || { children: [] }).children.map(c => ({
          selectIf: /^\s*function/.test(c.attrs.selectIf) ? fn(c.attrs.selectIf) : c.attrs.selectIf,
          structName: c.attrs.structName,
          fields: c.tag === 'group' ? fields(c.children) : { [c.attrs.name]: conv(c) },
        }));
        const defs = kids.find(c => c.tag === 'defaultFields');
        return common({ kind: 'tagged', fields: fields(kids), alternatives: alts, defaults: defs ? fields(defs.children) : {} }, a);
      }
    }
    throw new Error('unsupported OSD element ' + e.tag);
  };
  const listable = ['struct', 'array', 'bitfield', 'primitive', 'union', 'enum', 'flags', 'string'];
  return tree.children.filter(e => e.tag !== 'enumDef').map(e => ({ name: e.attrs.name, desc: conv(e), lock: e.attrs.defaultLockOffset, tag: e.tag, listable: listable.includes(e.tag) }));
}

// The checks DataInformationFactory makes while building a structure. Okteta
// drops a structure that fails any of them without showing it, so they're
// fatal here.
function checkDesc(desc, where, problems) {
  const sub = (d, name) => checkDesc(d, `${where}.${name}`, problems);
  const fields = obj => Object.keys(obj || {}).forEach(k => sub(obj[k], k));
  if (!desc || typeof desc !== 'object' || !desc.kind) { problems.push(`${where}: not a structure element`); return problems; }
  for (const f of ['validationFunc', 'toStringFunc', 'updateFunc'])
    if (desc[f] !== undefined && typeof desc[f] !== 'function') problems.push(`${where}: ${f} is not a function`);
  switch (desc.kind) {
    case 'prim':
      if (desc.type === 'bitfield' && !(desc.width >= 1 && desc.width <= 64)) problems.push(`${where}: bitfield width must be 1-64`);
      else if (desc.type !== 'bitfield' && !PRIM_SIZES[desc.type]) problems.push(`${where}: unrecognized primitive type ${desc.type}`);
      break;
    case 'enum':
      if (!desc.values || !Object.keys(desc.values).length) problems.push(`${where}: no enum values`);
      break;
    case 'array':
      if (desc.length === undefined || desc.length === '') problems.push(`${where}: no array length`);
      sub(desc.type, '<array type>');
      break;
    case 'struct': case 'union': fields(desc.fields); break;
    case 'pointer':
      if (!['uint8', 'uint16', 'uint32', 'uint64'].includes(desc.type && desc.type.type)) problems.push(`${where}: pointer type must be an unsigned integer`);
      if (desc.interpretFunc !== undefined && typeof desc.interpretFunc !== 'function') problems.push(`${where}: interpretFunc is not a function`);
      sub(desc.target, '<pointer target>');
      break;
    case 'tagged':
      if (!Object.keys(desc.fields || {}).length) problems.push(`${where}: no children in tagged union; at least one is required so that tag can be evaluated`);
      if (!Array.isArray(desc.alternatives) || !desc.alternatives.length) problems.push(`${where}: tagged union needs at least one alternative`);
      fields(desc.fields); fields(desc.defaults);
      (desc.alternatives || []).forEach((alt, i) => {
        if (typeof alt.selectIf !== 'function' && !/^\d+$/.test(String(alt.selectIf))) problems.push(`${where}: alternative ${i} selectIf is neither function nor number`);
        Object.keys(alt.fields || {}).forEach(k => sub(alt.fields[k], `<alternative ${i}>.${k}`));
      });
      break;
  }
  return problems;
}

function run(structure, file, addr, opts) {
  const problems = checkDesc(structure.desc, structure.name, []);
  if (structure.listable === false) problems.push(`${structure.name}: <${structure.tag}> can't be a top-level element; Okteta only lists struct, array, bitfield, primitive, union, enum, flags and string`);
  if (problems.length) return { top: null, ok: false, lines: [], errors: problems.map(p => 'FATAL (Okteta would drop this structure): ' + p) };

  input = fs.readFileSync(file);
  errors.length = 0;
  const top = new Node(structure.desc, structure.name, undefined);
  const r = readTop(top, addr);
  validate(top);
  return { top, ok: r >= 0, lines: dump(top, 0, [], opts), errors: errors.slice() };
}

// Checks what Okteta's plugin loading would trip over before any structure
// is read (StructureDefinitionFile, ScriptFileParser::parseStructureNames).
function checkPlugin(pluginDir) {
  const problems = [], id = path.basename(pluginDir);
  const meta = fs.readFileSync(path.join(pluginDir, 'metadata.desktop'), 'utf8');
  const field = k => (meta.match(new RegExp('^' + k + '=(.*)$', 'm')) || [])[1];
  const category = field('X-KDE-PluginInfo-Category');
  const hasJs = fs.existsSync(path.join(pluginDir, 'main.js'));
  const hasOsd = fs.existsSync(path.join(pluginDir, 'main.osd'));
  if (field('X-KDE-PluginInfo-Name') !== id)
    problems.push(`X-KDE-PluginInfo-Name should be the directory name "${id}"`);
  if (category === 'structure/js' && !hasJs) problems.push('category structure/js needs a main.js');
  if (category === 'structure' && !hasOsd) problems.push('category structure needs a main.osd (main.js is ignored)');
  if (category !== 'structure' && category !== 'structure/js') problems.push(`unknown category "${category}"`);
  if (category === 'structure/js' && hasJs) {
    const [s] = loadJs(path.join(pluginDir, 'main.js'), path.dirname(pluginDir));
    if (s.desc.name !== undefined && s.desc.name !== id)
      problems.push(`init() renames the structure to "${s.desc.name}"; Okteta looks it up as "${id}"`);
  }
  return problems.map(p => `${id}/metadata.desktop: ${p}`);
}

module.exports = { loadJs, loadOsd, run, checkPlugin, Node };
