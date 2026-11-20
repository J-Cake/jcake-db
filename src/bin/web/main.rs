#![feature(try_blocks)]
#![feature(formatting_options)]
extern crate core;

mod cluster;
mod http;
mod local_db;
mod util;

use crate::cluster::Cluster;
use crate::cluster::ClusterInner;
use crate::cluster::ClusterToken;
use crate::local_db::{open_db, DbHandle};
use crate::util::colours::ColouredStringBuilder;
use crate::util::parse_duration;
use axum::Json;
use axum::http::header;
use axum::middleware::Next;
use axum_core::response::IntoResponse;
use clap::Parser;
use http::method;
use hyper::StatusCode;
use serde_json::json;
use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::net::UnixListener;

#[derive(clap::Parser)]
struct Args {
	#[clap(long, default_value = "[::]:4560", env = "DB_BIND")]
	bind: Bind,

	#[clap(long, default_value = "./fs.db", env = "DB_DATABASE_PATH")]
	db: PathBuf,

	#[clap(short, long, env = "DB_EXTRACTOR")]
	extractor: String,

	#[command(subcommand)]
	mode: Option<Mode>,
}

#[derive(clap::Subcommand)]
enum Mode {
	/// Join (or found) a cluster
	Cluster(ClusterArgs),
}

#[derive(clap::Args)]
struct ClusterArgs {
	/// Join token: <secret_base64>@<cluster_fqdn>
	#[arg(long, env = "DB_CLUSTER_TOKEN", hide_env_values = true)]
	token: ClusterToken,

	/// Socket for cluster-internal traffic (gossip, forwarding)
	#[arg(long, default_value = "[::]:7700", env = "DB_GOSSIP_BIND")]
	gossip_bind: SocketAddr,

	#[arg(long, env = "DB_GOSSIP_ADVERTISE")]
	pub(crate) advertise_addr: Option<SocketAddr>,

	/// Number of copies per object (R)
	#[arg(long, default_value_t = 2, env = "DB_REPLICAS")]
	replicas: usize,

	/// How often to exchange member lists
	#[arg(long, default_value = "5s", value_parser = parse_duration, env = "DB_GOSSIP_INTERVAL")]
	gossip_interval: Duration,
}

#[derive(Debug, Clone)]
enum Bind {
	Socket(SocketAddr),
	Path(PathBuf),
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

#[tokio::main]
pub async fn main() -> io::Result<()> {
	let args = Args::parse();

	let db = open_db(&args).await?;

	match args.mode {
		None => bind_local(&args.bind, db).await?,
		Some(Mode::Cluster(cluster)) => bind_cluster(db, &args.bind, &cluster).await?
	};

	Ok(())
}

async fn bind_local(public: &Bind, db: DbHandle) -> io::Result<()> {
	serve_public(&public, app(db)).await?;

	Ok(())
}

async fn bind_cluster(db: DbHandle, public: &Bind, cluster: &ClusterArgs) -> io::Result<()> {
	let listener = TcpListener::bind(&cluster.gossip_bind).await?;

	util::info(format!("Starting gossip on {socket:?}", socket = &cluster.gossip_bind));

	let cluster = ClusterInner::new(&cluster, db.id())
		.await
		.map_err(|err| io::Error::other(err.to_string()))?;

	let gossip = Arc::clone(&cluster);
	let internal = app(db.clone());
	let app = app(db.into_cluster(gossip.clone()));

	tokio::spawn(async move { gossip.discovery().await });

	let gossip_router = axum::Router::new()
		.route("/discovery", method::post(cluster::discovery))
		.with_state(cluster.clone())
		.merge(internal)
		.layer(axum::middleware::from_fn_with_state(cluster, require_token));

	tokio::try_join!(serve_public(public, app), axum::serve(listener, gossip_router))?;

	Ok(())
}

fn app(db: crate::local_db::DbHandle) -> axum::Router {
	axum::Router::new()
		.route("/", method::query(http::query))
		.route("/", method::post(http::insert))
		.route("/{uuid}", method::get(http::fetch))
		.route("/{uuid}", method::put(http::upsert))
		.route("/{uuid}", method::delete(http::delete))
		.route("/health", method::get(async move || Json(json! {{
			"success": true,
			"healthy": true,
			"received": chrono::Utc::now()
		}})))
		.with_state(db.clone())
		.layer(axum::extract::DefaultBodyLimit::max(8_388_608)) // 8 MiB to account for JSON losses
		.layer(axum::middleware::from_fn(log))

}

async fn serve_public(bind: &Bind, app: axum::Router) -> io::Result<()> {
	util::info(format!("Starting webservice on {socket:?}", socket = bind));

	match bind {
		Bind::Path(path) => {
			let _ = tokio::fs::remove_file(path).await;
			#[cfg(not(target_family = "unix"))]
			panic!("Unix listeners are currently only supported on Unix. Windows support is coming soon.");

			axum::serve(UnixListener::bind(path)?, app).await
		}
		Bind::Socket(socket) => axum::serve(TcpListener::bind(socket).await?, app).await,
	}
}

async fn log(req: axum::extract::Request, next: Next) -> impl IntoResponse {
	let method = req.method().to_string();
	let path = req.uri().path().to_string();
	let res = next.run(req).await;

	let str = ColouredStringBuilder::default().now();

	let str = match res.status().as_u16() {
		200..300 => str.green(res.status().as_str()),
		300..400 => str.yellow(res.status().as_str()),
		400..500 => str.purple(res.status().as_str()),
		500..600 => str.red(res.status().as_str()),
		_ => str.normal(res.status().as_str()),
	};

	str.normal(" ").normal(method).normal(" ").grey(path).print();

	res
}

async fn require_token(state: axum::extract::State<Cluster>, req: axum::extract::Request, next: Next) -> impl IntoResponse {
	let ok = req
		.headers()
		.get(header::AUTHORIZATION)
		.and_then(|v| v.to_str().ok())
		.and_then(|v| v.strip_prefix("Bearer "))
		.is_some_and(|t| state.check_token(t))
		.into();

	if ok {
		Ok(next.run(req).await)
	} else {
		Err(StatusCode::UNAUTHORIZED)
	}
}
