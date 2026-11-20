// Okteta structure that walks a whole jcake-db file, starting at the
// superblock: page_offset leads to page 0, and each page links to its
// objects (one after another, each on a 16 byte boundary), its trailer and
// summary, and the next page.
//
// Okteta can only describe data that sits at fixed or pointed-to offsets, so
// every link here is a pointer whose target address is computed from the
// structure read so far (interpretFunc). The pointers sit in unions that
// overlay their parent's first byte, so they take up no space of their own;
// the byte value they show is meaningless, the computed target is shown
// instead. A target outside the file is skipped, which is how the walk ends.
//
// Needs ../jcake-db (for common.js) to be installed too.

var lib = importScript('jcake-db/common.js');

// How far the walk goes. Every level is built up front, so keep these modest.
var MAX_PAGES = 4;
var MAX_OBJECTS_PER_PAGE = 64;

// ---------------------------------------------------------------------------
// Building blocks

function magic(text) {
	var m = array(char(), 4);
	m.validationFunc = function() { return lib.checkMagic(this, text); };
	return m;
}

function uuid() {
	var u = array(uint8(), 16);
	u.typeName = 'UUID';
	u.toStringFunc = function() { return lib.uuidToString(this); };
	return u;
}

function timestamp() {
	var t = uint64();
	t.toStringFunc = function() { return lib.timestampToString(this); };
	return t;
}

// A pointer that takes no space of its own (it overlays one byte of a union)
// and whose target address is `address(pointer, root)`. The row shows the
// target address, or `describe(target)` once the target has been read, or
// `none` (default "(end)") when there is no target.
function link(target, address, describe, none) {
	var p = pointer(uint8(), target, 1, function(root) { return address(this, root); });
	p.typeName = 'link';
	p.toStringFunc = function(root) {
		var a = address(this, root);
		if (a == lib.NOWHERE) {
			return none || '(end)';
		}
		var s = '-> 0x' + a.toString(16);
		return describe && this.target.wasAbleToRead ? s + ': ' + describe(this.target) : s;
	};
	return p;
}

// ---------------------------------------------------------------------------
// Address arithmetic, by climbing back along the links that led here.
// Every page and object node is the target of exactly one pointer, so a
// node's parent is that pointer and the pointer's parent is the previous node.

function pageAddress(page, root) {
	var via = page.parent;
	if (via.name == 'page_offset') {
		return via.value;
	}
	return pageAddress(via.parent, root) + root.page_size.value; // via next_page
}

function pageOfObject(obj) {
	var via = obj.parent;
	return via.name == 'objects' ? via.parent : pageOfObject(via.parent);
}

// From one object's start to the next one's, alignment padding included.
function objectSize(obj) {
	return lib.objectStride(obj.header.length.value);
}

function objectAddress(obj, root) {
	var via = obj.parent;
	if (via.name == 'objects') {
		return pageAddress(via.parent, root) + lib.FIRST_OBJECT_OFFSET;
	}
	var prev = via.parent; // via next
	return objectAddress(prev, root) + objectSize(prev);
}

function trailerAddress(page, root) {
	return pageAddress(page, root) + root.page_size.value - lib.PAGE_FOOTER_SIZE;
}

function firstObject(ptr, root) {
	return pageAddress(ptr.parent, root) + lib.FIRST_OBJECT_OFFSET;
}

function nextObject(ptr, root) {
	var obj = ptr.parent, h = obj.header;
	if (!h.wasAbleToRead) {
		return lib.NOWHERE; // the header runs past the end of the file
	}
	if (lib.isFreeSpace(h)) {
		return lib.NOWHERE; // zeroes: padding or unused space after the last object
	}
	var kind = h.kind.value;
	if (kind > 4) {
		return lib.NOWHERE; // not an object header; don't chase garbage
	}
	var next = objectAddress(obj, root) + objectSize(obj);
	var limit = trailerAddress(pageOfObject(obj), root);
	return next + lib.OBJECT_HEADER_SIZE > limit ? lib.NOWHERE : next;
}

// The payload sits behind pointers so that a corrupt length (running past
// the end of the file) only makes the payload unreadable, not the header.
function payload(ptr, root) {
	var obj = ptr.parent, h = obj.header;
	if (!h.wasAbleToRead || lib.isFreeSpace(h)) {
		return lib.NOWHERE;
	}
	return objectAddress(obj, root) + lib.OBJECT_HEADER_SIZE;
}

function isJsonb(obj) {
	return obj.header.wasAbleToRead && obj.header.kind.value == 1; // Structured
}

function jsonPayload(ptr, root) {
	return isJsonb(ptr.parent) ? payload(ptr, root) : lib.NOWHERE;
}

function nextPage(ptr, root) {
	return pageAddress(ptr.parent, root) + root.page_size.value;
}

function trailer(ptr, root) {
	return trailerAddress(ptr.parent, root);
}

// A summary entry's offset is relative to its page:
// offset -> entry -> entries -> summary struct -> summary link -> trailer union -> trailer link -> page
function summaryTarget(ptr, root) {
	var page = ptr.parent.parent.parent.parent.parent.parent.parent;
	return pageAddress(page, root) + ptr.value;
}

function summary(ptr, root) {
	var t = ptr.parent, n = t.trailer.num_items.value;
	if (n == 0 || !lib.hasMagic(t.trailer.magic, 'foot')) {
		return lib.NOWHERE;
	}
	var page = t.parent.parent;
	return trailerAddress(page, root) - lib.PAGE_SUMMARY_HEADER_SIZE - n * lib.PAGE_SUMMARY_ENTRY_SIZE;
}

// ---------------------------------------------------------------------------
// Objects (src/object.rs), see main.osd's "Object record"

function objectHeaderFields() {
	return struct({
		uuid: uuid(),
		length: bitfield('unsigned', 24),
		kind: enumeration('ObjectKind', uint8(), {
			'none': 0,
			'Structured': 1,
			'Raw': 2,
			'Tombstone': 3,
			'Reference': 4
		}),
		crc: uint32()
	});
}

// The CRC covers the payload, which sits behind the object's `data` link.
function objectHeader() {
	var h = objectHeaderFields();
	h.validationFunc = function() {
		var data = this.parent.data.target, payload = null;
		if (this.length.value == 0) {
			payload = []; // nothing to read, possibly right at the end of the file
		} else if (data.wasAbleToRead) {
			payload = lib.pushArray([], data);
		}
		return lib.validateObjectHeader(this, payload);
	};
	return h;
}

// Okteta needs a tagged union to have fixed children to select on, and a
// payload has none, so instead of choosing between JSONB and raw bytes each
// object links to both: `data` always, `json` only when the payload is JSONB.
function rawPayload() {
	// data -> link -> object
	return array(uint8(), function() { return this.parent.parent.header.length.value; });
}

function jsonbPayload() {
	var container = uint32();
	container.toStringFunc = function() { return lib.describeContainer(this.value); };
	var jsonb = struct({
		container: container,
		jentries: array(uint32(), function() {
			return lib.jsonbEntryCount(this.parent.container.value);
		}),
		data: array(uint8(), function() {
			var obj = this.parent.parent.parent; // jsonb, link, object
			var n = obj.header.length.value - 4 - 4 * this.parent.jentries.length;
			return n > 0 ? n : 0;
		})
	});
	jsonb.byteOrder = 'bigEndian';
	jsonb.toStringFunc = function() { return lib.jsonbStructToJson(this); };
	return jsonb;
}

function objectNode(index) {
	var fields = {
		header: objectHeader(),
		data: link(rawPayload(), payload, function(d) { return d.length + ' bytes'; }, '(none)'),
		json: link(jsonbPayload(), jsonPayload, lib.jsonbStructToJson, '(not JSONB)')
	};
	if (index + 1 < MAX_OBJECTS_PER_PAGE) {
		fields.next = link(objectNode(index + 1), nextObject);
	}
	var node = union(fields);
	node.toStringFunc = function() {
		var h = this.header;
		if (!h.wasAbleToRead) {
			return '(header runs past the end of the file)';
		}
		if (lib.isFreeSpace(h)) {
			return '(free space)';
		}
		return lib.uuidToString(h.uuid) + ', ' + h.length.value + ' bytes';
	};
	return node;
}

// ---------------------------------------------------------------------------
// Pages (src/page.rs)

function pageTrailer() {
	var entry = struct({
		uuid: uuid(),
		offset: pointer(uint32(), objectHeaderFields(), 1, function(root) { return summaryTarget(this, root); })
	});
	entry.validationFunc = function() {
		var header = this.offset.target;
		return lib.validateSummaryEntry(this, header.wasAbleToRead ? header : null);
	};
	var smry = struct({
		magic: magic('smry'),
		entries: array(entry, function() { return this.parent.parent.parent.trailer.num_items.value; })
	});
	var foot = struct({ magic: magic('foot'), num_items: uint32(), crc: uint32() });
	foot.validationFunc = function() { return lib.validateTrailer(this); };
	return union({ trailer: foot, summary: link(smry, summary) });
}

function pageNode(n) {
	var header = struct({
		magic: magic('page'),
		seq: uint64(),
		page_size: uint32(),
		created: timestamp(),
		crc: uint32()
	});
	header.validationFunc = function() { return lib.validatePageHeader(this); };

	var fields = {
		header: header,
		objects: link(objectNode(0), firstObject),
		trailer: link(pageTrailer(), trailer)
	};
	if (n + 1 < MAX_PAGES) {
		fields.next_page = link(pageNode(n + 1), nextPage);
	}
	var page = union(fields);
	page.toStringFunc = function() { return 'page ' + this.header.seq.value; };
	return page;
}

// ---------------------------------------------------------------------------
// Superblock (src/file.rs)

function init() {
	var sb = struct({
		magic: magic('fsdb'),
		uuid: uuid(),
		version: uint32(),
		page_size: uint32(),
		page_offset: pointer(uint32(), pageNode(0)),
		crc: uint32()
	});
	// No name override: Okteta lists a JS structure under its plugin id
	// (jcake-db-walk) and finds it by that name when it's enabled.
	sb.byteOrder = 'littleEndian';
	sb.defaultLockOffset = 0;
	sb.validationFunc = function() { return lib.validateSuperblock(this); };
	return sb;
}
