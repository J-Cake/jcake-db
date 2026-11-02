# JCake DB

> I wrote 100% of the code. Claude wrote bits that I used for testing like the Okteta stuff. If you can look past that, 
> it's a handwritten project.  

I like databases. I find them fascinating. I wanted to create my own Document Database inspired by SQLite - an on-disk
and file-based database.

The database is centred around JSONB using and attempts to mimic many of the features of CouchDB.

Using this database is possible, but I'm honestly not sure that you'd want to. It's a hobby project because I'm
interested in it. But if you do decide to try it, good luck! Please write to me, I'd love to know about your use-case.
That said, it should be stated clearly that **this database requires reliable storage**. There is zero error-correction,
write-ahead logging, or atomicity guarantees build into the database (it's a hobby database, not sure how clear I can
make that.) My recommendation is a normal file on disk, but I personally have gotten it working via iSCSI backed by
mirrored volumes on SSDs. your mileage **will** vary.

## What this database does not do

1. Protect your data (Encryption, crash-safety, regular backups etc)
2. Provide any sort of authentication system
3. Come with enterprise support
4. Feed you Müsli or wash your car

## What it does do instead

This database implements a copy-on-wrote (CoW) strategy for writing data. The database is divided into records
(internally referred to as objets) that upon write, get duplicated entirely in their modified form and appended to the
end of the database. By regularly indexing the contents of chunks of data (int. pages), the database can be quite
performant at lookups and writes. (Dare I say constant time)

Records are JSON (JSONB, actually) payloads that can take any form you like. Note that records are internally limited to
4MiB by an adjustable parameter, but be careful when adjusting this, as you may run into page size limitations of 16MiB
(which you can also adjust)

## Getting started

> there's a few examples in the `./examples` directory. Check those out for a few end-to-end examples.

Probably the main struct you'll be interacting with is the `TypedDatabase` struct. It provides a convenient wrapper
around the `ObjectStore` trait that automatically serialises and deserialises (using `serde`) values when
reading/writing data.

```rust
#[derive(Debug, Serialize, Deserialize)]
struct MaybeTyped<'a, T: Deserialize<'a> = serde_json::Value> {
	#[serde(default)]
	kind: Option<String>,
	
	#[serde(flat)]
	data: T
};

#[derive(Debug, Serialize, Deserialize)]
struct Document {
	owner: String
}

let mut db = TypedDatabase::new("./example.db")?;
    .with_extractor("Index on object kind", |record: MaybeTyped| record.kind)
    .with_extractor("Project owner", |record: MaybeTyped<Document>| record.kind.and_then(|_| record.data.owner));

// The database automatically builds indices for basically everything. 
// The `query` function uses these indices to return a list of objects matching certain criteria 
// (limited of course to values that were indexed to begin with).
db.query(serde_json::json! {{
	"kind": "project",
	"owner": "john.doe@email.com"
}})?;

// Insert a new project file
db.insert(serde_json::json! {{
	"kind": "project",
	"owner": "john.doe@email.com",
	"name": "Example Project",
	"created": "2026-10-01T12:00:30.000Z",
	"data": {
		// ... this bit isn't relevant, trust me it's an example. 
	}
}})?;
```