#![feature(gethostname)]

mod cluster;
mod db;
mod http;
mod util;

use hyper::StatusCode;
use crate::util::{now, parse_duration};
use axum::Json;
use axum::middleware::Next;
use clap::Parser;
use http::method;
use serde_json::json;
use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use axum::http::header;
use axum_core::response::IntoResponse;
use tokio::net::TcpListener;
use tokio::net::UnixListener;
use crate::cluster::{Cluster, ClusterToken};
use crate::cluster::ClusterInner;
use crate::util::colours::ColouredStringBuilder;

#[derive(clap::Parser)]
struct Args {
    #[clap(long, default_value = "0.0.0.0:4560", env = "DB_BIND")]
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
    #[arg(long, default_value = "0.0.0.0:7700", env = "DB_GOSSIP_BIND")]
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

    let db = db::open_db(&args).await?;
    let id = db.id();

    let app = axum::Router::new()
        .route("/", method::query(http::query))
        .route("/", method::post(http::insert))
        .route("/{uuid}", method::get(http::fetch))
        .route("/{uuid}", method::put(http::upsert))
        .route("/{uuid}", method::delete(http::delete))
        .route("/health", method::get(async move || {
            Json(json! {{
                "success": true,
                "healthy": true,
                "received": chrono::Utc::now()
            }})
        }))
        .with_state(db)
        .layer(axum::extract::DefaultBodyLimit::max(8_388_608)) // 8 MiB to account for JSON losses
        .layer(axum::middleware::from_fn(log));

    util::info(format!("Starting webservice on {socket:?}", socket=&args.bind));

    let gossip = match args.mode {
        None => None,
        Some(Mode::Cluster(args)) => {
            let listener = TcpListener::bind(&args.gossip_bind).await?;
            let cluster = ClusterInner::new(&args, id).await
                .map_err(|err| io::Error::other(err.to_string()))?;

            let gossip = Arc::clone(&cluster);
            tokio::spawn(async move {
                gossip.discovery().await
            });

            Some((listener, axum::Router::new()
                .route("/discovery", method::post(cluster::discovery))
                .with_state(cluster.clone())
                .merge(app.clone())
                .layer(axum::middleware::from_fn_with_state(cluster, require_token))))
        }
    };

    tokio::try_join!(serve_public(&args.bind, app), async {
        match gossip {
            Some((bind, gossip)) => axum::serve(bind, gossip).await,
            None => Ok(())
        }
    })?;

    Ok(())
}

async fn serve_public(bind: &Bind, app: axum::Router) -> io::Result<()> {
    match bind {
        Bind::Path(path) => {
            let _ = tokio::fs::remove_file(path).await?;
            axum::serve(UnixListener::bind(path)?, app).await
        },
        Bind::Socket(socket) => axum::serve(TcpListener::bind(socket).await?, app).await,
    }
}

async fn log(req: axum::extract::Request, next: Next) -> impl IntoResponse {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let res = next.run(req).await;

    let str = ColouredStringBuilder::default()
        .now();

    let str = match res.status().as_u16() {
        200..300 => str.green(res.status().as_str()),
        300..400 => str.yellow(res.status().as_str()),
        400..500 => str.purple(res.status().as_str()),
        500..600 => str.red(res.status().as_str()),
        _ => str.normal(res.status().as_str())
    };

    str.normal(method)
        .normal(" ")
        .grey(path)
        .print();

    res
}

async fn require_token(state: axum::extract::State<Cluster>, req: axum::extract::Request, next: Next) -> impl IntoResponse {
    let ok = req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| state.check_token(t)).into();

    if ok { Ok(next.run(req).await) } else { Err(StatusCode::UNAUTHORIZED) }
}