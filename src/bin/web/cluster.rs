use std::collections::BTreeMap;
use std::fmt::Display;
use std::io;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use axum::Json;
use axum_core::response::IntoResponse;
use base64::Engine;
use clap::error::ErrorKind;
use hyper::Uri;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;
use base64::engine::general_purpose::STANDARD as base64_standard;
use chrono::Utc;
use tokio::net::UdpSocket;
use crate::ClusterArgs;
use crate::util::colours::ColouredStringBuilder;

#[derive(Clone)]
pub(crate) struct ClusterToken {
	cluster_id: String,
	secret: Box<[u8]>
}

impl FromStr for ClusterToken {
	type Err = clap::error::Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let Some((token, cluster)) = s.split_once('@') else {
			return Err(Self::Err::new(ErrorKind::InvalidValue))
		};

		Ok(Self {
			cluster_id: cluster.to_string(),
			secret: base64_standard.decode(token)
				.map_err(|_| Self::Err::raw(ErrorKind::InvalidValue, "Invalid token"))?
				.into()
		})
	}
}

impl std::fmt::Debug for ClusterToken {
	fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
		f.debug_struct("ClusterToken")
			.field("cluster_id", &self.cluster_id)
			.field("secret", &"<redacted>")
			.finish()
	}
}

#[derive(Debug)]
pub(crate) enum ClusterError {
	DnsError(String)
}

impl Display for ClusterError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			ClusterError::DnsError(err) => write!(f, "DNS Error: {err}")
		}
	}
}

pub(crate) type Result<T, E = ClusterError> = core::result::Result<T, E>;

pub(crate) type Cluster = Arc<ClusterInner>;

pub(crate) struct ClusterInner {
	id: Uuid,
	token: ClusterToken,
	discover_interval: Duration,
	member_list: RwLock<BTreeMap<Uuid, Member>>,
	http: crate::util::HttpClient,
	ad: SocketAddr,
}


impl ClusterInner {
	async fn add_member(&self, member: Member) {
		if member.uuid != self.id {
			self.member_list
				.write()
				.await
				.entry(member.uuid)
				.and_modify(|existing| existing.merge(&member))
				.or_insert(member);
		}
	}

	pub(crate) async fn new(args: &ClusterArgs, id: Uuid) -> Result<Cluster> {
		let ad = get_advertising_addr(args).await
			.expect("Failed to get advertising address.");

		Ok(Arc::new(Self {
			id,
			ad,
			token: args.token.clone(),
			discover_interval: args.gossip_interval,
			member_list: RwLock::new(BTreeMap::from([
				(id, Member {
					uuid: id,
					epoch_µs: Utc::now().timestamp_micros() as u64,
					addr: ad,
					left: false,
				})
			])),
			http: crate::util::client(),
		}))
	}

	async fn join_request(&self) -> Discovery {
		Discovery {
			known_peers: self.member_list.read()
				.await
				.values()
				.cloned()
				.collect(),
			own_id: self.id
		}
	}

	pub(crate) fn check_token(&self, token: &str) -> bool {
		if token.len() <= 2 {
			return false;
		}

		if token.len() <= 16 {
			eprintln!("\x1b[33mWARNING:\x1b[m The token is less than 16 bytes long. This is not recommended in production scenarios.");
		}

		let Ok(bytes) = base64_standard.decode(token) else {
			return false;
		};

		crate::util::ct_eq(&self.token.secret, &bytes)
	}

	async fn update_member_list(&self) -> Result<()> {
		let ips = tokio::net::lookup_host((self.token.cluster_id.as_str(), self.ad.port())).await
			.map_err(|err| ClusterError::DnsError(err.to_string()))?;

		let join = self.join_request().await;

		for name in ips {
			let Ok(uri) = Uri::builder()
				.scheme("http")
				.authority(name.to_string())
				.path_and_query("/discover")
				.build() else {
				continue;
			};

			let Ok(res) = crate::util::post_json::<_, Vec<Member>>(&self.http, uri, &join).await else {
				continue;
			};

			for member in res {
				self.add_member(member).await
			}
		}

		Ok(())
	}

	pub(crate) async fn discovery(&self) -> Result<()> {
		loop {
			if let Err(err) = self.update_member_list().await {
				ColouredStringBuilder::default()
					.now()
					.red("Error: ")
					.normal(err.to_string())
					.eprint();
			}
			tokio::time::sleep(self.discover_interval).await;
		}
	}
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Member {
	uuid: Uuid,
	#[serde(rename = "epoch")]
	epoch_µs: u64,
	addr: SocketAddr,
	left: bool,
}

impl Member {
	fn merge(&mut self, other: &Member) {
		self.left = self.left || other.left;

		if other.epoch_µs > self.epoch_µs {
			self.epoch_µs = other.epoch_µs;
			self.addr = other.addr;
		}
	}
}

async fn get_advertising_addr(args: &ClusterArgs) -> io::Result<SocketAddr> {
	if let Some(ad) = args.advertise_addr {
		return Ok(ad);
	}

	let bind = args.gossip_bind;
	if bind.port() == 0 {
		return Err(io::Error::other("--gossip-bind needs a fixed port"));
	}
	if !bind.ip().is_unspecified() {
		return Ok(bind);
	}

	for peer in tokio::net::lookup_host((args.token.cluster_id.as_str(), bind.port())).await? {
		let socket = UdpSocket::bind(if peer.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).await?;
		if socket.connect(peer).await.is_err() {
			continue;
		}

		let ip = socket.local_addr()?.ip();
		if !ip.is_unspecified() && !ip.is_loopback() {
			return Ok(SocketAddr::new(ip, bind.port()));
		}
	}

	Err(io::Error::other("Couldn't work out this node's address. Pass --advertise <ip:port>."))
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Discovery {
	known_peers: Vec<Member>,
	own_id: Uuid
}

pub(crate) async fn discovery(cluster: axum::extract::State<Cluster>, body: Json<Discovery>) -> impl IntoResponse {
	for peer in body.0.known_peers {
		cluster.add_member(peer).await
	}

	(hyper::StatusCode::OK, Json(cluster.member_list.read().await.values()
		.collect::<Vec<_>>()))
		.into_response()
}