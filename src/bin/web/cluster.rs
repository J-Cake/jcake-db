use std::collections::BTreeMap;
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
use crate::ClusterArgs;

#[derive(Clone)]
pub(crate) struct ClusterToken {
	cluster_id: String,
	secret: Box<[u8]>
}

impl FromStr for ClusterToken {
	type Err = clap::error::Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let Some((cluster, token)) = s.split_once('.') else {
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

// fn parse_hex(s: &str) -> Option<Box<[u8]>> {
// 	if s.len() % 2 != 0 { return None; }
// 	(0..s.len()).step_by(2)
// 		.map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
// 		.collect()
// }

fn valid_label(s: &str) -> bool {
	(1..=63).contains(&s.len())
		&& s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
		&& !s.starts_with('-') && !s.ends_with('-')
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

pub(crate) type Result<T, E = ClusterError> = core::result::Result<T, E>;

pub(crate) type Cluster = Arc<ClusterInner>;

pub(crate) struct ClusterInner {
	token: ClusterToken,

	discover_interval: Duration,

	member_list: RwLock<BTreeMap<Uuid, Member>>,

	http: crate::util::HttpClient
}

impl ClusterInner {
	async fn add_member(&self, member: Member) {
		self.member_list
			.write()
			.await
			.entry(member.uuid)
			.and_modify(|existing| existing.merge(&member))
			.or_insert(member);
	}
}

#[axum::debug_handler]
pub(crate) async fn discovery(cluster: axum::extract::State<Cluster>, body: Json<Discovery>) -> impl IntoResponse {
	for peer in body.0.known_peers {
		cluster.add_member(peer).await
	}

	(hyper::StatusCode::OK, Json(cluster.member_list.read().await.values()
		.map(|member| Member {
			epoch_µs: Utc::now().timestamp_micros() as u64,
			..*member
		})
		.collect::<Vec<_>>()))
		.into_response()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Member {
	uuid: Uuid,
	#[serde(rename = "epoch")]
	epoch_µs: u64,
	addr: SocketAddr,
	left: bool
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

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Discovery {
	known_peers: Vec<Member>
}

impl ClusterInner {
	pub(crate) fn new(args: &ClusterArgs) -> Cluster {
		Arc::new(Self {
			token: args.token.clone(),
			discover_interval: args.gossip_interval,

			member_list: Default::default(),

			http: crate::util::client()
		})
	}

	fn join_request(&self) -> Discovery {
		Discovery {
			known_peers: vec![],
		}
	}

	pub(crate) fn check_token(&self, token: &str) -> bool {
		let Ok(bytes) = base64_standard.decode(token) else {
			return false;
		};

		crate::util::ct_eq(&self.token.secret, &bytes)
	}

	async fn update_member_list(&self) -> Result<()> {
		let ips = tokio::net::lookup_host(&self.token.cluster_id).await
			.map_err(|err| ClusterError::DnsError(err.to_string()))?;

		let join = self.join_request();

		for name in ips {
			let Ok(uri) = Uri::builder()
				.authority(name.to_string())
				.path_and_query("/gossip")
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
			self.update_member_list().await?;
			tokio::time::sleep(self.discover_interval).await;
		}
	}
}