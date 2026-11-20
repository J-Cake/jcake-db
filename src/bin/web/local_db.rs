use crate::Args;
use crate::cluster::Cluster;
use crate::util::http::RequestArgs;
use axum::http::uri::Scheme;
use db::TypedDatabase;
use hyper::{header, Method};
use hyper::Uri;
use jaq_core::Compiler;
use jaq_core::Ctx;
use jaq_core::Vars;
use jaq_core::data;
use jaq_core::load::Arena;
use jaq_core::load::File;
use jaq_core::load::Loader;
use jaq_core::unwrap_valr;
use jaq_json::Val;
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::json;
use std::io;
use std::io::{Error};
use axum::http::{HeaderMap, HeaderValue};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use util::traits::ToBytes;
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct DbHandleInner {
    sender: UnboundedSender<Request>,
    id: Uuid,
}

#[must_use]
pub(crate) enum WriteOutcome<T, Err = io::Error> {
    Success(T),
    Degraded {
        success: usize,
        replicas: usize,
        value: T,
        errors: Vec<Err>,
    },
    Failure(Err),
}

impl<T, Err: std::fmt::Debug> WriteOutcome<T, Err> {
    pub(crate) fn into_result(self) -> Result<T, Err> {
        match self {
            Self::Success(value) => Ok(value),
            Self::Degraded { value, errors, success, replicas } => {
                crate::util::warning(format!("Operation failed for {f}/{t} clients: {errors:?}", f=replicas-success, t=replicas));
                Ok(value)
            }
            Self::Failure(err) => Err(err),
        }
    }
}

#[derive(Clone)]
pub(crate) enum DbHandle {
    Local { db: DbHandleInner },
    Cluster { db: DbHandleInner, cluster: Cluster },
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
            DbHandle::Local { db } | DbHandle::Cluster { db, .. } => db.id,
        }
    }

    pub(crate) fn into_cluster(self, cluster: Cluster) -> Self {
        match self {
            Self::Cluster { db, .. } | Self::Local { db } => Self::Cluster { db, cluster },
        }
    }

    async fn msg<Ret: DeserializeOwned>(&self, req: RequestIntention) -> WriteOutcome<Ret> {
        let (tx, rx) = oneshot::channel::<db::Result<Value>>();

        let req = Request {
            inner: req,
            res: tx,
        };

        if let Err(_) = self.sender().send(req) {
            return WriteOutcome::Failure(io::Error::other("Failed to dispatch request"));
        };

        let Ok(Ok(res)) = rx.await else {
            return WriteOutcome::Failure(io::Error::other(
                "An unknown error occurred awaiting a response from the database",
            ));
        };

        match serde_json::from_value(res) {
            Ok(val) => WriteOutcome::Success(val),
            Err(err) => WriteOutcome::Failure(io::Error::other(err)),
        }
    }

    pub(crate) async fn query(&self, query: impl ToBytes + Serialize) -> WriteOutcome<Vec<Value>> {
        if let DbHandle::Cluster { cluster, .. } = self {
            // Collect results from all members
            let mut set = JoinSet::<io::Result<Vec<Value>>>::new();
            let members = cluster.members().await;
            let num = members.len();

            let body = match serde_json::to_vec(&query) {
                Ok(body) => axum::body::Bytes::from(body),
                Err(err) => return WriteOutcome::Failure(io::Error::other(format!("Could not serialise query: {err}"))),
            };

            for member in members {
                let query = query.to_bytes();
                let handle = self.clone();
                let cluster = cluster.clone();
                let body = body.clone();

                set.spawn(async move {
                    let http = cluster.http();

                    if member.uuid == handle.id() {
                        handle.msg(RequestIntention::Query(query)).await.into_result()
                    } else {
                        let uri = Uri::builder()
                            .scheme(Scheme::HTTP)
                            .authority(member.addr.to_string())
                            .path_and_query("/")
                            .build()
                            .map_err(|err| io::Error::other(err.to_string()))?;

                        let res = http
                            .req(RequestArgs {
                                uri,
                                method: Method::QUERY,
                                headers: json_headers(),
                                body: Some(axum::body::Body::from(body))
                            })
                            .await;

                        match res {
                            Ok(res) =>
                                res.into_json().await.map_err(|err| Error::other(err.to_string())),
                            Err(err) => Err(Error::other(err.to_string())),
                        }
                    }
                });
            }

            let mut success = 0usize;
            let mut results = vec![];
            let mut errors = vec![];

            for outcome in set.join_all().await {
                match outcome {
                    Ok(result) => {
                        success += 1;
                        results.extend_from_slice(&result);
                    }
                    Err(err) => errors.push(err),
                }
            }

            if errors.is_empty() {
                WriteOutcome::Success(results)
            } else if success == 0 && !errors.is_empty() {
                WriteOutcome::Failure(io::Error::other(format!("All clients failed to respond: {errors:?}")))
            } else {
                WriteOutcome::Degraded {
                    success,
                    errors,
                    replicas: num,
                    value: results,
                }
            }
        } else {
            self.msg(RequestIntention::Query(query.to_bytes())).await
        }
    }

    pub(crate) async fn fetch(&self, uuid: Uuid) -> WriteOutcome<Option<Value>> {
        if let DbHandle::Cluster { cluster, .. } = self {
            for owner in cluster.get_owners(uuid).await {
                if owner.uuid == self.id() {
                    continue;
                }

                let Ok(uri) = Uri::builder()
                    .scheme(Scheme::HTTP)
                    .authority(owner.addr.to_string())
                    .path_and_query(format!("/{uuid}"))
                    .build()
                    .inspect_err(|err| {
                        crate::util::warning(format!("Constructing URL failed: {err:?}"))
                    })
                else {
                    continue;
                };

                match cluster.http().get_json(uri).await {
                    Ok(Some(res)) => return WriteOutcome::Success(Some(res)),
                    Ok(None) => return WriteOutcome::Success(None), // The owner must have it. If we successfully receive nothing, there's no point checking the other owners.
                    Err(err) => crate::util::warning(format!(
                        "Fetching from {addr} failed: {err:?}",
                        addr = owner.addr
                    )),
                }
            }

            WriteOutcome::Success(None)
        } else {
            self.msg(RequestIntention::Fetch(uuid)).await
        }
    }

    pub(crate) async fn insert(&self, obj: Value) -> WriteOutcome<Uuid> {
        if let DbHandle::Cluster { cluster, .. } = self {
            let uuid = Uuid::new_v4();

            match self.upsert(uuid, obj).await {
                WriteOutcome::Success(_) => WriteOutcome::Success(uuid),
                WriteOutcome::Degraded {
                    success,
                    replicas,
                    errors,
                    ..
                } => WriteOutcome::Degraded {
                    success,
                    replicas,
                    errors,
                    value: uuid,
                },
                WriteOutcome::Failure(err) => WriteOutcome::Failure(err),
            }
        } else {
            self.msg(RequestIntention::Insert(obj)).await
        }
    }

    pub(crate) async fn upsert(&self, uuid: Uuid, obj: Value) -> WriteOutcome<()> {
        if let DbHandle::Cluster { cluster, .. } = self {
            let mut join = JoinSet::new();

            let body = match serde_json::to_string(&obj) {
                Ok(body) => body,
                Err(err) => return WriteOutcome::Failure(err.into())
            };

            for owner in cluster.get_owners(uuid).await {
                let handle = self.clone();
                let obj = obj.clone();
                let cluster = cluster.clone();
                let body = body.clone();

                join.spawn(async move {
                    if owner.uuid == handle.id() {
                        handle.msg(RequestIntention::Upsert(uuid, obj)).await.into_result()
                    } else {
                        let uri = Uri::builder()
                            .scheme(Scheme::HTTP)
                            .authority(owner.addr.to_string())
                            .path_and_query(format!("/{uuid}"))
                            .build()
                            .map_err(|err| io::Error::other(err.to_string()))?;

                        let res = cluster
                            .http()
                            .req(RequestArgs {
                                method: Method::PUT,
                                uri,
                                body: Some(axum::body::Body::new(body)),
                                headers: json_headers()
                            })
                            .await
                            .map_err(|err| Error::other(format!("Insert at {peer} failed: {err}", peer = owner.addr)))?;

                        if res.is_success() {
                            Ok(())
                        } else {
                            match res.into_text().await {
                                Ok(err) => Err(Error::other(err)),
                                Err(err) => Err(Error::other(err.to_string())),
                            }
                        }
                    }
                });
            }

            let mut success = 0usize;
            let mut errors = vec![];

            for outcome in join.join_all().await {
                match outcome {
                    Ok(()) => success += 1,
                    Err(err) => errors.push(err),
                }
            }

            if errors.is_empty() {
                WriteOutcome::Success(())
            } else if success == 0 && !errors.is_empty() {
                WriteOutcome::Failure(io::Error::other(format!(
                    "All clients failed to update: {errors:?}"
                )))
            } else {
                WriteOutcome::Degraded {
                    success,
                    errors,
                    replicas: cluster.replicas(),
                    value: (),
                }
            }
        } else {
            self.msg(RequestIntention::Upsert(uuid, obj)).await
        }
    }

    pub(crate) async fn delete(&self, uuid: Uuid) -> WriteOutcome<()> {
        if let DbHandle::Cluster { cluster, .. } = self {
            let mut join = JoinSet::new();

            for owner in cluster.get_owners(uuid).await {
                let handle = self.clone();
                let cluster = cluster.clone();

                join.spawn(async move {
                    if owner.uuid == handle.id() {
                        handle.msg(RequestIntention::Delete(uuid)).await.into_result()
                    } else {
                        let uri = Uri::builder()
                            .scheme(Scheme::HTTP)
                            .authority(owner.addr.to_string())
                            .path_and_query(format!("/{uuid}"))
                            .build()
                            .map_err(|err| io::Error::other(err.to_string()))?;

                        let res = cluster
                            .http()
                            .req(RequestArgs {
                                uri,
                                method: Method::DELETE,
                                ..Default::default()
                            })
                            .await;

                        match res {
                            Ok(_) => Ok(()),
                            Err(err) => Err(io::Error::other(err.to_string())),
                        }
                    }
                });
            }

            let mut success = 0usize;
            let mut errors = vec![];
            for outcome in join.join_all().await {
                match outcome {
                    Ok(()) => success += 1,
                    Err(err) => errors.push(err),
                }
            }

            if errors.is_empty() {
                WriteOutcome::Success(())
            } else if success == 0 && !errors.is_empty() {
                WriteOutcome::Failure(io::Error::other(format!(
                    "All clients failed to delete: {errors:?}"
                )))
            } else {
                WriteOutcome::Degraded {
                    success,
                    errors,
                    replicas: cluster.replicas(),
                    value: (),
                }
            }
        } else {
            self.msg(RequestIntention::Delete(uuid)).await
        }
    }
}

fn json_headers() -> HeaderMap {
    let mut m = HeaderMap::new();
    m.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));

    return m;
}

struct Request {
    inner: RequestIntention,
    res: oneshot::Sender<db::Result<Value>>,
}

pub(crate) enum RequestIntention {
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

        id_tx.send(db.id()).expect("Failed to send DB ID");

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
        },
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
