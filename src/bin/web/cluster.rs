use crate::header::HeaderMap;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::io;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use axum::http::HeaderValue;
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
use format::Error;
use util::Crc;
use crate::{headers, ClusterArgs};
use crate::local_db::RequestIntention;
use crate::util::colours::ColouredStringBuilder;
use crate::util::http::{HttpClient, RequestError};
use crate::util::MaxN;

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

		if cluster.len() > 253 || !cluster.strip_suffix('.')
			.unwrap_or(cluster)
			.split('.')
			.all(|name| {
				(1..=63).contains(&name.len())
					&& name.bytes().all(|char| char.is_ascii_alphanumeric() || char == b'-')
					&& !name.starts_with('-')
					&& !name.ends_with('-')
			}) {
			return Err(clap::error::Error::raw(ErrorKind::InvalidValue, ClusterError::invalid_cluster_name(cluster)))
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

#[derive(Debug, Error)]
pub(crate) enum ClusterError {
	DnsError(String),
	InitialisationError(RequestError),
	TokenError(String),
	AdvertisingAddressError(String),
	InvalidClusterName(String)
}

impl ClusterError {
	fn token_error(msg: impl AsRef<str>) -> Self {
		Self::TokenError(msg.as_ref().to_string())
	}

	fn malformed_ad_addr(msg: impl AsRef<str>) -> Self {
		Self::AdvertisingAddressError(msg.as_ref().to_string())
	}

	fn invalid_cluster_name(msg: impl AsRef<str>) -> Self {
		Self::InvalidClusterName(msg.as_ref().to_string())
	}
}

impl Display for ClusterError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			ClusterError::InitialisationError(err) => write!(f, "Error during cluster initialisation: {err}"),
			ClusterError::DnsError(err) => write!(f, "DNS Error: {err}"),
			ClusterError::TokenError(err) => write!(f, "Invalid Token: {err}"),
			ClusterError::AdvertisingAddressError(err) => write!(f, "Malformed Advertising Address: {err}"),
			ClusterError::InvalidClusterName(err) => write!(f, "Cluster name not a valid DNS host: {err}")
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
	http: crate::util::http::HttpClient,
	ad: SocketAddr,
	replicas: usize,
}

impl ClusterInner {
	pub(crate) async fn new(args: &ClusterArgs, id: Uuid) -> Result<Cluster> {
		if args.token.secret.len() < 8 {
			return Err(ClusterError::token_error("Too Short"))
		}

		if args.token.secret.len() < 16 {
			ColouredStringBuilder::default()
				.now()
				.yellow("Warning: ")
				.normal("The token is less than 16 bytes long. This is not recommended in production scenarios.")
				.eprint()
		}

		clock_sync().await;

		let ad = get_advertising_addr(args).await
			.map_err(|err| ClusterError::malformed_ad_addr(err.to_string()))?;

		let mut header = HeaderValue::from_str(&format!("Bearer {secret}", secret=base64_standard.encode(&args.token.secret)))
			.map_err(|err| return ClusterError::token_error(err.to_string()))?;

		header.set_sensitive(true);

		Ok(Arc::new(Self {
			id,
			ad,
			replicas: args.replicas,
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
			http: crate::util::http::HttpClient::new()
				.with_headers(headers! {
					"Authorization": &format!("Bearer {token}", token=base64_standard.encode(&args.token.secret))
				}.map_err(|err| ClusterError::InitialisationError(RequestError::InvalidHeaderError(err)))?),
		}))
	}

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

	pub(crate) async fn members(&self) -> Vec<Member> {
		self.member_list.read().await.values().cloned().collect()
	}

	pub(crate) async fn get_owners(&self, uuid: Uuid) -> Vec<Member> {
		// `max_by_key` returns the last-found maximum of all that are found.
		// Since this is a BTreeMap, the objects are ordered by UUID already, making them
		// have the same order on all nodes and provides a reliable tie-breaker.
		// DO NOT swap the backing structure for a HashMap without considering this.
		// TODO: Factorise this to allow for replication to take the top N
		self.member_list
			.read().await
			.values()
			.filter(|i| !i.left)
			.cloned()
			.max_n_by_key(self.replicas, |member| {
				let mut rank = Crc::new();

				rank.update(member.uuid.as_bytes());
				rank.update(uuid.as_bytes());

				let mut rank = u32::from_le_bytes(rank.finish());

				// MurmurHash3
				rank ^= rank >> 16;
				rank = rank.wrapping_mul(0x85eb_ca6b);
				rank ^= rank >> 13;
				rank = rank.wrapping_mul(0xc2b2_ae35);

				rank ^ (rank >> 16)
			})
	}

	pub(crate) fn http(&self) -> &HttpClient {
		&self.http
	}

	async fn join_request(&self) -> Discovery {
		Discovery {
			known_peers: self.member_list.read()
				.await
				.values()
				.cloned()
				.collect(),
		}
	}

	pub(crate) fn check_token(&self, token: &str) -> bool {
		let Ok(bytes) = base64_standard.decode(token) else {
			return false;
		};

		crate::util::ct_eq(&self.token.secret, &bytes)
	}

	async fn update_member_list(&self) -> Result<()> {
		let ips = tokio::net::lookup_host((self.token.cluster_id.as_str(), self.ad.port())).await
			.map_err(|err| ClusterError::DnsError(err.to_string()))?;

		#[cfg(debug_assertions)]
		let ips = {
			let ips = ips.collect::<Vec<_>>();
			ColouredStringBuilder::default()
				.now()
				.cyan("Debug: ")
				.dbg(&ips)
				.eprint();
			ips
		};

		let join = self.join_request().await;

		for name in ips {
			let Ok(uri) = Uri::builder()
				.scheme("http")
				.authority(name.to_string())
				.path_and_query("/discovery")
				.build() else {
				continue;
			};

			let Ok(res) = self.http.post_json::<_, Discovery>(uri, &join).await
				.inspect_err(|err| crate::util::error(err.to_string())) else {
				continue;
			};

			for member in res.known_peers {
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
	pub(crate) uuid: Uuid,
	#[serde(rename = "epoch")]
	epoch_µs: u64,
	pub(crate) addr: SocketAddr,
	left: bool,
}

impl Member {
	fn merge(&mut self, other: &Member) {
		self.left = self.left || other.left;

		if other.epoch_µs > self.epoch_µs {

			#[cfg(debug_assertions)]
			crate::util::info(format!("Discovered new member: {ip} ({uuid})", ip=other.addr.ip(), uuid=other.uuid));

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
	// own_id: Uuid
}

pub(crate) async fn discovery(cluster: axum::extract::State<Cluster>, body: Json<Discovery>) -> impl IntoResponse {
	for peer in body.0.known_peers {
		cluster.add_member(peer).await
	}

	(hyper::StatusCode::OK, Json(serde_json::json! {{
		"known_peers": cluster.member_list.read().await // Instead of a `Discovery`, saves a clone.
			.values()
			.collect::<Vec<_>>()
	}}))
		.into_response()
}

async fn clock_sync() {
	match clock_synced() {
		Some(true) => {}
		Some(false) => {
			ColouredStringBuilder::default()
				.now()
				.yellow("Warning: ")
				.normal("System clock isn't synced yet. Waiting up to 5s.")
				.eprint();

			let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
			while tokio::time::Instant::now() < deadline && clock_synced() == Some(false) {
				tokio::time::sleep(Duration::from_millis(500)).await;
			}

			if clock_synced() != Some(true) {
				ColouredStringBuilder::default()
					.now()
					.yellow("Warning: ")
					.normal("Clock still not synced. Continuing anyway.")
					.eprint();
			}
		}
		None => {
			ColouredStringBuilder::default()
				.now()
				.yellow("Warning: ")
				.normal("Can't determine whether the system clock is synced on this platform.")
				.eprint();
		}
	}
}

#[cfg(target_os = "linux")]
fn clock_synced() -> Option<bool> {
	let mut tx: libc::timex = unsafe { std::mem::zeroed() }; // modes = 0: only read
	let state = unsafe { libc::adjtimex(&mut tx) };
	(state >= 0).then_some(state != libc::TIME_ERROR)
}

#[cfg(not(target_os = "linux"))]
fn clock_synced() -> Option<bool> {
	None
}