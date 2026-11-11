use std::convert::Infallible;
use std::io;
use std::io::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use axum::response::IntoResponse;
use clap::Parser;
use jaq_core::{unwrap_valr, Compiler, Ctx, Vars, data};
use jaq_core::load::{Arena, File, Loader};
use jaq_json::Val;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::oneshot;
use uuid::Uuid;
use db::{keys, TypedDatabase};
use util::traits::ToBytes;

#[derive(clap::Parser)]
struct Args {
	#[clap(default_value = "0.0.0.0:4560", env = "DB_BIND")]
	bind: Bind,

	#[clap(default_value = "./fs.db", env = "DB_DATABASE_PATH")]
	db: PathBuf,

	#[clap(short, long, env = "DB_EXTRACTOR")]
	extractor: String
}

#[derive(Clone)]
enum Bind {
	Socket(SocketAddr),
	Path(PathBuf)
}

impl FromStr for Bind {
	type Err = Infallible;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s.parse::<SocketAddr>() {
			Ok(addr) => Bind::Socket(addr),
			Err(_) => Bind::Path(PathBuf::from(s)),
		})
	}
}

mod method {
	use axum::extract::{Request, State};
	use axum::handler::Handler;
	use axum::http::StatusCode;
	use axum::response::IntoResponse;
	pub use axum::routing::*;

	pub fn query<H, T, S>(handler: H) -> MethodRouter<S>
	where
		H: Handler<T, S>,
		T: 'static,
		S: Clone + Send + Sync + 'static,
	{
		MethodRouter::new().fallback(move |State(state): State<S>, req: Request| async move {
			if req.method().as_str() == "QUERY" {
				handler.call(req, state).await
			} else {
				StatusCode::METHOD_NOT_ALLOWED.into_response()
			}
		})
	}
}

#[tokio::main]
pub async fn main() -> io::Result<()> {
	let args = Args::parse();

	let db = open_db(&args).await?;

	let app = axum::Router::new()
		.route("/", method::query(query))
		.route("/", method::post(insert))
		.route("/{uuid}", method::get(fetch))
		.route("/{uuid}", method::put(upsert))
		.route("/{uuid}", method::delete(delete))

		.route("/health", method::get(async move || Json(json! {{
			"success": true,
			"healthy": true,
			"received": chrono::Utc::now()
		}})))
		.with_state(db);

	match &args.bind {
		Bind::Path(path) => {
			let server = UnixListener::bind(path)?;
			axum::serve(server, app)
				.await?;
		},
		Bind::Socket(socket) => {
			let server = TcpListener::bind(socket).await?;
			axum::serve(server, app)
				.await?;
		}
	};

	Ok(())
}

#[axum::debug_handler]
async fn query(db: State<DbHandle>, body: Json<String>) -> impl IntoResponse {
	match db.query(body.0).await {
		Ok(data) => (StatusCode::OK, Json(data))
			.into_response(),
		Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
			.into_response()
	}
}

#[axum::debug_handler]
async fn fetch(path: Path<(Uuid,)>, db: State<DbHandle>) -> impl IntoResponse {
	match db.fetch(path.0.0).await {
		Ok(Some(obj)) => (StatusCode::OK, Json(obj))
			.into_response(),
		Ok(None) => (StatusCode::NOT_FOUND, Json(format!("No object {uuid}", uuid=path.0.0)))
			.into_response(),
		Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
			.into_response()
	}
}

#[axum::debug_handler]
async fn insert(db: State<DbHandle>, body: Json<Value>) -> impl IntoResponse {
	match db.insert(body.0).await {
		Ok(uuid) => (StatusCode::OK, Json(uuid))
			.into_response(),
		Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
			.into_response()
	}
}

#[axum::debug_handler]
async fn upsert(path: Path<(Uuid,)>, db: State<DbHandle>, body: Json<Value>) -> impl IntoResponse {
	match db.upsert(path.0.0, body.0).await {
		Ok(()) => (StatusCode::OK, Json(()))
			.into_response(),
		Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
			.into_response()
	}
}

#[axum::debug_handler]
async fn delete(path: Path<(Uuid,)>, db: State<DbHandle>) -> impl IntoResponse {
	match db.delete(path.0.0).await {
		Ok(()) => (StatusCode::OK, Json(()))
			.into_response(),
		Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
			.into_response()
	}
}

#[derive(Clone)]
struct DbHandle {
	sender: UnboundedSender<Request>
}

impl DbHandle {
	async fn msg<Ret: DeserializeOwned>(&self, req: RequestIntention) -> io::Result<Ret> {
		let (tx, rx) = oneshot::channel::<db::Result<Value>>();

		let req = Request {
			inner: req,
			res: tx
		};

		if let Err(_) = self.sender.send(req) {
			return Err(io::Error::other("Failed to dispatch request"));
		};

		let Ok(Ok(res)) = rx.await else {
			return Err(io::Error::other("An unknown error occurred awaiting a response from the database"));
		};

		return serde_json::from_value(res)
			.map_err(|err| Error::other(err))
	}

	async fn query(&self, query: impl ToBytes) -> io::Result<Vec<Value>> {
		self.msg(RequestIntention::Query(query.to_bytes()))
			.await
	}

	async fn fetch(&self, uuid: Uuid) -> io::Result<Option<Value>> {
		self.msg(RequestIntention::Fetch(uuid))
			.await
	}

	async fn insert(&self, obj: Value) -> io::Result<Uuid> {
		self.msg(RequestIntention::Insert(obj))
			.await
	}

	async fn upsert(&self, uuid: Uuid, obj: Value) -> io::Result<()> {
		self.msg(RequestIntention::Upsert(uuid, obj))
			.await
	}

	async fn delete(&self, uuid: Uuid) -> io::Result<()> {
		self.msg(RequestIntention::Delete(uuid))
			.await
	}
}

struct Request {
	inner: RequestIntention,
	res: oneshot::Sender<db::Result<Value>>
}

enum RequestIntention {
	Query(Box<[u8]>),
	Fetch(Uuid),
	Insert(Value),
	Upsert(Uuid, Value),
	Delete(Uuid),
}

async fn open_db(args: &Args) -> io::Result<DbHandle> {
	let (tx, mut rx) = unbounded_channel::<Request>();
	let db = args.db.to_owned();

	let mapper = args.extractor.clone();

	std::thread::Builder::new()
		.name("db-worker".into())
		.spawn(move || {
			let mapper = prepare_mapper(&mapper)
				.expect("Invalid extractor function");

			let mut db = TypedDatabase::new(db)
				.expect("Failed to open database")
				.with_mapper(mapper);

			while let Some(req) = rx.blocking_recv() {
				if let Err(_) = match req.inner {
					RequestIntention::Query(query) => {
						let res = db.query::<Value>(query);
						req.res.send(res.map(Value::from))
					},
					RequestIntention::Fetch(uuid) => {
						let res = db.fetch::<Value>(uuid);
						req.res.send(res.map(Value::from))
					},
					RequestIntention::Insert(obj) => {
						let res = db.insert(obj);
						req.res.send(res.map(|uuid| json! {{ "uuid": uuid }}))
					}
					RequestIntention::Upsert(uuid, obj) => {
						let res = db.upsert(uuid, obj);
						req.res.send(res.map(Value::from))
					}
					RequestIntention::Delete(uuid) => {
						let res = db.delete(uuid);
						req.res.send(res.map(Value::from))
					}
				} { dbg!("Dispatch failed."); };
			}
		})?;

	Ok(DbHandle {
		sender: tx
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
		filter.id
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