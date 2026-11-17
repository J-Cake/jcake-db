use std::io;
use std::io::Error;
use jaq_core::data;
use jaq_core::Compiler;
use jaq_core::Ctx;
use jaq_core::Vars;
use jaq_core::unwrap_valr;
use jaq_core::load::Arena;
use jaq_core::load::File;
use jaq_core::load::Loader;
use jaq_json::Val;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use serde_json::Value;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use uuid::Uuid;
// use db::TypedDatabase;
use util::traits::ToBytes;
use crate::Args;
use crate::cluster::Cluster;

#[derive(Clone)]
pub(crate) struct DbHandleInner {
	sender: UnboundedSender<Request>,
	id: Uuid
}

#[derive(Clone)]
pub(crate) enum DbHandle {
	Local {
		db: DbHandleInner,
	},
	Cluster {
		db: DbHandleInner,
		cluster: Cluster
	}
}

impl DbHandle {
	fn sender(&self) -> UnboundedSender<Request> {
		match self {
			DbHandle::Local { db } => db.sender.clone(),
			DbHandle::Cluster { db, .. } => db.sender.clone(),
		}
	}

	pub(crate) fn id(&self) -> Uuid {
		match self {
			DbHandle::Local { db } | DbHandle::Cluster { db, .. } => db.id
		}
	}

	fn into_cluster(self, cluster: Cluster) -> Self {
		match self {
			Self::Cluster { db, .. } | Self::Local { db } => Self::Cluster {
				db,
				cluster
			}
		}
	}

	async fn msg<Ret: DeserializeOwned>(&self, req: RequestIntention) -> io::Result<Ret> {
		let (tx, rx) = oneshot::channel::<db::Result<Value>>();

		let req = Request {
			inner: req,
			res: tx,
		};

		if let Err(_) = self.sender().send(req) {
			return Err(io::Error::other("Failed to dispatch request"));
		};

		let Ok(Ok(res)) = rx.await else {
			return Err(io::Error::other(
				"An unknown error occurred awaiting a response from the database",
			));
		};

		return serde_json::from_value(res).map_err(|err| Error::other(err));
	}

	pub(crate) async fn query(&self, query: impl ToBytes) -> io::Result<Vec<Value>> {
		self.msg(RequestIntention::Query(query.to_bytes())).await
	}

	pub(crate) async fn fetch(&self, uuid: Uuid) -> io::Result<Option<Value>> {
		self.msg(RequestIntention::Fetch(uuid)).await
	}

	pub(crate) async fn insert(&self, obj: Value) -> io::Result<Uuid> {
		self.msg(RequestIntention::Insert(obj)).await
	}

	pub(crate) async fn upsert(&self, uuid: Uuid, obj: Value) -> io::Result<()> {
		self.msg(RequestIntention::Upsert(uuid, obj)).await
	}

	pub(crate) async fn delete(&self, uuid: Uuid) -> io::Result<()> {
		self.msg(RequestIntention::Delete(uuid)).await
	}
}

struct Request {
	inner: RequestIntention,
	res: oneshot::Sender<db::Result<Value>>,
}

enum RequestIntention {
	Query(Box<[u8]>),
	Fetch(Uuid),
	Insert(Value),
	Upsert(Uuid, Value),
	Delete(Uuid),
}

pub(crate) async fn open_db(args: &Args) -> io::Result<DbHandle> {
	let (tx, mut rx) = unbounded_channel::<Request>();
	let db = args.db.to_owned();

	let mapper = args.extractor.clone();
	let (id_tx, id_rx) = oneshot::channel();

	std::thread::Builder::new().name("db-worker".into()).spawn(move || {
		let mapper = prepare_mapper(&mapper).expect("Invalid extractor function");

		let mut db = TypedDatabase::new(db).expect("Failed to open database").with_mapper(mapper);

		id_tx.send(db.id())
			.expect("Failed to send DB ID");

		while let Some(req) = rx.blocking_recv() {
			if let Err(_) = match req.inner {
				RequestIntention::Query(query) => {
					let res = db.query::<Value>(query);
					req.res.send(res.map(Value::from))
				}
				RequestIntention::Fetch(uuid) => {
					let res = db.fetch::<Value>(uuid);
					req.res.send(res.map(Value::from))
				}
				RequestIntention::Insert(obj) => {
					let res = db.insert(obj);
					req.res.send(res.map(|uuid| json!(uuid)))
				}
				RequestIntention::Upsert(uuid, obj) => {
					let res = db.upsert(uuid, obj);
					req.res.send(res.map(Value::from))
				}
				RequestIntention::Delete(uuid) => {
					let res = db.delete(uuid);
					req.res.send(res.map(Value::from))
				}
			} {
				dbg!("Dispatch failed.");
			};
		}
	})?;

	Ok(DbHandle::Local {
		db: DbHandleInner {
			sender: tx,
			id: id_rx.await.map_err(|err| Error::other(err.to_string()))?,
		}
	})
}

fn prepare_mapper(code: &str) -> Result<impl Fn(&Value) -> Vec<Box<[u8]>> + 'static, String> {
	let defs = jaq_core::defs().chain(jaq_std::defs()).chain(jaq_json::defs());
	let funs = jaq_core::funs().chain(jaq_std::funs()).chain(jaq_json::funs());

	let loader = Loader::new(defs);
	let arena = Arena::default();
	let modules = loader
		.load(&arena, File { code, path: () })
		.map_err(|e| format!("jq parse error: {e:?}"))?;
	let filter = Compiler::default()
		.with_funs(funs)
		.compile(modules)
		.map_err(|e| format!("jq compile error: {e:?}"))?;

	Ok(move |obj: &Value| {
		// &serde_json::Value is itself a serde Deserializer, so no clone needed
		let input = match Val::deserialize(obj) {
			Ok(v) => v,
			Err(err) => {
				eprintln!("extractor: could not convert object: {err}");
				return vec![];
			}
		};

		let ctx = Ctx::<data::JustLut<Val>>::new(&filter.lut, Vars::new([]));
		filter
			.id
			.run((ctx, input))
			.map(unwrap_valr)
			.filter_map(|out| match out {
				Ok(v) => Some(val_to_key(&v)),
				Err(err) => {
					eprintln!("extractor: filter error: {err}");
					None
				}
			})
			.collect()
	})
}

fn val_to_key(v: &Val) -> Box<[u8]> {
	// Back through serde_json so the same ToBytes impls as typed keys are used
	let text = v.to_string();
	match serde_json::from_str::<Value>(&text) {
		Ok(Value::String(s)) => s.to_bytes().into(),
		Ok(Value::Bool(b)) => b.to_bytes().into(),
		Ok(Value::Number(n)) => match n.as_i64() {
			Some(i) => i.to_bytes().into(),
			None => n.as_f64().unwrap_or_default().to_bytes().into(),
		},
		// null/arrays/objects as keys: fall back to their JSON text
		_ => text.into_bytes().into_boxed_slice(),
	}
}
