// Shared helpers for the jcake-db Okteta structures.
//
// Loaded from main.osd and ../jcake-db-walk/main.js with
// importScript('jcake-db/common.js'), so this file has to be installed next
// to them under okteta/structures/. Okteta's script engine is QtScript
// (ES5): no let/const, arrow functions, typed arrays or BigInt.

// ---------------------------------------------------------------------------
// Constants mirrored from src/page.rs

var PAGE_HEADER_SIZE = 28;
var PAGE_FOOTER_SIZE = 12;
var PAGE_SUMMARY_HEADER_SIZE = 4;
var PAGE_SUMMARY_ENTRY_SIZE = 20;
var OBJECT_HEADER_SIZE = 24;         // what ObjectHeader::read_at_position expects
var OBJECT_HEADER_SIZE_WRITTEN = 20; // what ObjectHeader::write_struct emits

// A pointer target beyond any file; Okteta silently skips such pointers.
var NOWHERE = 0xFFFFFFFF;

// ---------------------------------------------------------------------------
// Byte gathering

function pushLE(bytes, value, n) {
    for (var i = 0; i < n; i++) {
        bytes.push((value >>> (8 * i)) & 0xFF);
    }
    return bytes;
}

function pushBE(bytes, value, n) {
    for (var i = n - 1; i >= 0; i--) {
        bytes.push((value >>> (8 * i)) & 0xFF);
    }
    return bytes;
}

function pushU64LE(bytes, prim) {
    pushLE(bytes, prim.uint64low32, 4);
    return pushLE(bytes, prim.uint64high32, 4);
}

// Arrays of primitives hand out one shared element object, so each element
// must be read before the next one is fetched.
function pushArray(bytes, arr) {
    var n = arr.length;
    for (var i = 0; i < n; i++) {
        bytes.push(arr[i].uint8);
    }
    return bytes;
}

// ---------------------------------------------------------------------------
// CRC-32C (src/crc.rs); stored on disk as a little-endian u32

function crc32c(bytes) {
    var c = 0xFFFFFFFF;
    for (var i = 0; i < bytes.length; i++) {
        c ^= bytes[i];
        for (var k = 0; k < 8; k++) {
            c = (c & 1) ? ((c >>> 1) ^ 0x82F63B78) : (c >>> 1);
        }
    }
    return (~c) >>> 0;
}

function hex32(v) {
    var s = (v >>> 0).toString(16);
    while (s.length < 8) {
        s = '0' + s;
    }
    return '0x' + s;
}

// Sets a validation message on `data` and returns whether the CRC matched.
function checkCrc(data, bytes, stored, what) {
    var expected = crc32c(bytes);
    if (expected == stored) {
        return true;
    }
    data.validationError = what + ': stored ' + hex32(stored) + ', computed ' + hex32(expected);
    return false;
}

function hasMagic(arr, text) {
    for (var i = 0; i < text.length; i++) {
        if (arr[i].uint8 != text.charCodeAt(i)) {
            return false;
        }
    }
    return true;
}

// Only for validationFunc: validationError is read-only at any other time.
function checkMagic(arr, text) {
    if (hasMagic(arr, text)) {
        return true;
    }
    arr.validationError = 'expected "' + text + '"';
    return false;
}

// ---------------------------------------------------------------------------
// Display helpers

// Uuid::from_slice keeps the bytes in order, so this matches Uuid's Display.
function uuidToString(arr) {
    var h = '0123456789abcdef', s = '';
    for (var i = 0; i < 16; i++) {
        var v = arr[i].uint8;
        s += h.charAt(v >> 4) + h.charAt(v & 15);
        if (i == 3 || i == 5 || i == 7 || i == 9) {
            s += '-';
        }
    }
    return s;
}

function isNilUuid(arr) {
    for (var i = 0; i < 16; i++) {
        if (arr[i].uint8 != 0) {
            return false;
        }
    }
    return true;
}

// chrono timestamp_micros stored as u64
function timestampToString(prim) {
    var us = prim.uint64high32 * 4294967296 + prim.uint64low32;
    return new Date(us / 1000).toUTCString() + ' (' + us + ' µs)';
}

// ---------------------------------------------------------------------------
// Structure validators

// src/file.rs: CRC over the first 32 bytes
function validateSuperblock(sb) {
    var b = pushArray([], sb.magic);
    pushArray(b, sb.uuid);
    pushLE(b, sb.version.value, 4);
    pushLE(b, sb.page_size.value, 4);
    pushLE(b, sb.page_offset.value, 4);
    return checkCrc(sb, b, sb.crc.value, 'superblock CRC');
}

// src/page.rs Page::write_struct / read_at_position: CRC over the first 24 bytes
function validatePageHeader(h) {
    var b = pushArray([], h.magic);
    pushU64LE(b, h.seq);
    pushLE(b, h.page_size.value, 4);
    pushU64LE(b, h.created);
    return checkCrc(h, b, h.crc.value, 'page header CRC');
}

// The object header is a tagged union: when `kind` is 0 the bytes came from
// ObjectHeader::write_struct (uuid + u32 length, no kind, no CRC), otherwise
// they are the 24-byte layout ObjectHeader::read_at_position parses.
function objectHeaderIsWrittenLayout(h) {
    return h.kind.value == 0;
}

function objectHeaderSize(h) {
    return objectHeaderIsWrittenLayout(h) ? OBJECT_HEADER_SIZE_WRITTEN : OBJECT_HEADER_SIZE;
}

// Zeroes where a header would be: the unused space between the last object
// and the summary of a sealed (zero padded) page.
function isFreeSpace(h) {
    return h.length.value == 0 && h.kind.value == 0 && isNilUuid(h.uuid);
}

function validateObjectHeader(h) {
    if (isFreeSpace(h)) {
        return true;
    }
    if (objectHeaderIsWrittenLayout(h)) {
        h.validationError = 'write_struct layout: only uuid + u32 length (20 bytes) were written, '
            + 'but read_at_position expects uuid, u24 length, kind and CRC (24 bytes)';
        return false;
    }
    var kind = h.kind.value;
    if (kind < 1 || kind > 4) {
        h.validationError = 'invalid object kind ' + kind;
        return false;
    }
    var b = pushArray([], h.uuid);
    pushLE(b, h.length.value, 3);
    b.push(kind);
    return checkCrc(h, b, h.crc.value, 'header CRC over bytes 0..20');
}

// Trailer CRC as PageTrailer::write_struct computes it: over the summary (if
// any) followed by "foot" and num_items. `summary` may be null.
function validateTrailer(t, summary) {
    var b = [];
    if (summary) {
        pushArray(b, summary.magic);
        var entries = summary.entries;
        for (var i = 0; i < entries.length; i++) {
            pushArray(b, entries[i].uuid);
            pushLE(b, entries[i].offset.value, 4);
        }
    }
    pushArray(b, t.magic);
    pushLE(b, t.num_items.value, 4);
    return checkCrc(t, b, t.crc.value, 'trailer CRC');
}

// ---------------------------------------------------------------------------
// Databend JSONB (jsonb 0.5.6, src/core/databend), all big-endian

var JSONB_ARRAY = 0x80000000;
var JSONB_OBJECT = 0x40000000;
var JSONB_SCALAR = 0x20000000;

function be32(b, i) {
    if (i + 4 > b.length) {
        throw 'truncated at byte ' + i;
    }
    return ((b[i] << 24) >>> 0) + (b[i + 1] << 16) + (b[i + 2] << 8) + b[i + 3];
}

function containerType(header) {
    return (header & 0xE0000000) >>> 0;
}

// Number of JEntries that follow a container header.
function jsonbEntryCount(header) {
    var t = containerType(header), n = header & 0x1FFFFFFF;
    return t == JSONB_OBJECT ? 2 * n : t == JSONB_SCALAR ? 1 : n;
}

function describeContainer(header) {
    var t = containerType(header), n = header & 0x1FFFFFFF;
    if (t == JSONB_SCALAR) {
        return 'scalar';
    }
    if (t == JSONB_OBJECT) {
        return 'object, ' + n + (n == 1 ? ' key' : ' keys');
    }
    if (t == JSONB_ARRAY) {
        return 'array, ' + n + (n == 1 ? ' element' : ' elements');
    }
    return 'invalid container header ' + hex32(header);
}

function describeJEntry(je) {
    var names = ['null', 'string', 'number', 'false', 'true', 'container', 'extension', 'invalid'];
    var s = names[(je >>> 28) & 7] + ', ' + (je & 0x0FFFFFFF) + ' bytes';
    return (je & 0x80000000) ? s + ' (offset mode, unsupported)' : s;
}

// Exact decimal rendering of a big-endian two's-complement (or unsigned)
// integer of any width, by long division on 16-bit limbs.
function bigIntToDecimal(b, start, len, signed) {
    var negative = signed && (b[start] & 0x80) != 0;
    var bytes = [];
    if (len % 2) {
        bytes.push(negative ? 0xFF : 0); // sign-extend to whole 16-bit limbs
    }
    for (var i = 0; i < len; i++) {
        bytes.push(b[start + i]);
    }
    var limbs = [];
    for (var j = 0; j < bytes.length; j += 2) {
        limbs.push((bytes[j] << 8) | bytes[j + 1]);
    }
    if (negative) {
        // two's complement negate
        var c = 1;
        for (var n = limbs.length - 1; n >= 0; n--) {
            var x = ((~limbs[n]) & 0xFFFF) + c;
            limbs[n] = x & 0xFFFF;
            c = x >>> 16;
        }
    }
    var digits = '';
    for (;;) {
        var zero = true, rem = 0;
        for (var p = 0; p < limbs.length; p++) {
            var cur = rem * 65536 + limbs[p];
            limbs[p] = Math.floor(cur / 10);
            rem = cur % 10;
            if (limbs[p]) {
                zero = false;
            }
        }
        digits = rem + digits;
        if (zero) {
            break;
        }
    }
    return (negative ? '-' : '') + digits;
}

function decodeF64(b, i) {
    var hi = be32(b, i), lo = be32(b, i + 4);
    var sign = (hi >>> 31) ? -1 : 1, exp = (hi >>> 20) & 0x7FF;
    var mant = (hi & 0xFFFFF) * 4294967296 + lo;
    if (exp == 0x7FF) {
        return mant ? NaN : sign * Infinity;
    }
    if (exp == 0) {
        return sign * mant * Math.pow(2, -1074);
    }
    return sign * (1 + mant / 4503599627370496) * Math.pow(2, exp - 1023);
}

function numberToJson(b, start, end) {
    var len = end - start - 1, tag = b[start];
    switch (tag) {
    case 0x00: return '0';
    case 0x10: return 'NaN';
    case 0x20: return 'Infinity';
    case 0x30: return '-Infinity';
    case 0x40:
    case 0x50:
        if (len != 1 && len != 2 && len != 4 && len != 8) {
            throw 'bad integer width ' + len;
        }
        return bigIntToDecimal(b, start + 1, len, tag == 0x40);
    case 0x60:
        return String(decodeF64(b, start + 1));
    case 0x70: {
        // value, then scale; the 18 and 34 byte forms carry a precision byte first
        var valueLen = { 9: 8, 17: 16, 18: 16, 33: 32, 34: 32 }[len];
        if (!valueLen) {
            throw 'bad decimal width ' + len;
        }
        var scale = b[end - 1];
        var digits = bigIntToDecimal(b, start + 1, valueLen, true), neg = digits.charAt(0) == '-';
        if (neg) {
            digits = digits.substring(1);
        }
        while (digits.length <= scale) {
            digits = '0' + digits;
        }
        var s = scale ? digits.substring(0, digits.length - scale) + '.' + digits.substring(digits.length - scale) : digits;
        return (neg ? '-' : '') + s;
    }
    default:
        throw 'bad number tag 0x' + tag.toString(16);
    }
}

function utf8ToString(b, start, end) {
    var esc = '';
    for (var i = start; i < end; i++) {
        esc += '%' + (b[i] < 16 ? '0' : '') + b[i].toString(16);
    }
    try {
        return decodeURIComponent(esc);
    } catch (e) {
        throw 'invalid UTF-8 in string at byte ' + start;
    }
}

function quoteJson(s) {
    var out = '"';
    for (var i = 0; i < s.length; i++) {
        var c = s.charAt(i), code = s.charCodeAt(i);
        if (c == '"' || c == '\\') {
            out += '\\' + c;
        } else if (c == '\n') {
            out += '\\n';
        } else if (c == '\r') {
            out += '\\r';
        } else if (c == '\t') {
            out += '\\t';
        } else if (code < 0x20) {
            out += '\\u' + ('000' + code.toString(16)).slice(-4);
        } else {
            out += c;
        }
    }
    return out + '"';
}

function extensionToJson(b, start, end) {
    var tag = b[start], body = [];
    for (var i = start + 1; i < end; i++) {
        body.push((b[i] < 16 ? '0' : '') + b[i].toString(16));
    }
    var names = { 0x00: 'binary', 0x10: 'date', 0x20: 'timestamp', 0x30: 'timestamp_tz', 0x40: 'interval' };
    return quoteJson('<' + (names[tag] || 'extension 0x' + tag.toString(16)) + ' ' + body.join('') + '>');
}

function valueToJson(b, je, start, end) {
    if (je & 0x80000000) {
        throw 'offset-mode JEntry ' + hex32(je) + ' is not supported by jsonb 0.5.6';
    }
    if (end > b.length) {
        throw 'value runs past the end of the payload';
    }
    switch ((je & 0x70000000) >>> 0) {
    case 0x00000000: return 'null';
    case 0x10000000: return quoteJson(utf8ToString(b, start, end));
    case 0x20000000: return numberToJson(b, start, end);
    case 0x30000000: return 'false';
    case 0x40000000: return 'true';
    case 0x50000000: return containerToJson(b, start, end);
    case 0x60000000: return extensionToJson(b, start, end);
    default: throw 'bad JEntry type ' + hex32(je);
    }
}

function containerToJson(b, start, end) {
    var header = be32(b, start), t = containerType(header), n = header & 0x1FFFFFFF;
    var entries = jsonbEntryCount(header), jentries = [];
    for (var i = 0; i < entries; i++) {
        jentries.push(be32(b, start + 4 + 4 * i));
    }
    var off = start + 4 + 4 * entries, parts = [];
    if (t == JSONB_SCALAR) {
        return valueToJson(b, jentries[0], off, off + (jentries[0] & 0x0FFFFFFF));
    }
    if (t == JSONB_ARRAY) {
        for (var a = 0; a < n; a++) {
            var alen = jentries[a] & 0x0FFFFFFF;
            parts.push(valueToJson(b, jentries[a], off, off + alen));
            off += alen;
        }
        return '[' + parts.join(',') + ']';
    }
    if (t == JSONB_OBJECT) {
        // all keys' data first, then all values' data
        var keys = [];
        for (var k = 0; k < n; k++) {
            var klen = jentries[k] & 0x0FFFFFFF;
            keys.push(valueToJson(b, jentries[k], off, off + klen));
            off += klen;
        }
        for (var v = 0; v < n; v++) {
            var vlen = jentries[n + v] & 0x0FFFFFFF;
            parts.push(keys[v] + ':' + valueToJson(b, jentries[n + v], off, off + vlen));
            off += vlen;
        }
        return '{' + parts.join(',') + '}';
    }
    throw 'bad container header ' + hex32(header);
}

// JSON text for a JSONB byte array, or a description of why it isn't valid.
function jsonbToJson(bytes) {
    if (bytes.length < 4) {
        return '<empty JSONB>';
    }
    try {
        return containerToJson(bytes, 0, bytes.length);
    } catch (e) {
        return '<invalid JSONB: ' + e + '>';
    }
}

// The same for a decoded `jsonb` structure (container, jentries, data).
function jsonbStructToJson(s) {
    var b = pushBE([], s.container.value, 4);
    var je = s.jentries;
    for (var i = 0; i < je.length; i++) {
        pushBE(b, je[i].value, 4);
    }
    pushArray(b, s.data);
    return jsonbToJson(b);
}
