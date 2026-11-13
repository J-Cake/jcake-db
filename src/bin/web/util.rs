use std::{net::SocketAddr, time::Duration};

use axum::{
	body::Body,
	extract::Request,
	http::{header, uri::{PathAndQuery, Scheme}, HeaderValue, Uri},
	response::Response,
};
use chrono::Local;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper_util::{client::legacy::{connect::HttpConnector, Client}, rt::TokioExecutor};
use serde::{de::DeserializeOwned, Serialize};

pub(crate) type HttpClient = Client<HttpConnector, Body>;
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub(crate) fn client() -> HttpClient {
	Client::builder(TokioExecutor::new())
		.pool_idle_timeout(Duration::from_secs(90))
		.build_http()
}

/// http://<addr><path>, IPv6 gets its brackets automatically
pub(crate) fn peer_uri(addr: SocketAddr, path: &str) -> Result<Uri> {
	Ok(Uri::builder()
		.scheme(Scheme::HTTP)
		.authority(addr.to_string())
		.path_and_query(path)
		.build()?)
}

pub(crate) async fn post_json<T: Serialize, R: DeserializeOwned>(
	client: &HttpClient, uri: Uri, body: &T,
) -> Result<R> {
	let req = Request::post(uri)
		.header(header::CONTENT_TYPE, "application/json")
		.body(Body::from(serde_json::to_vec(body)?))?;

	read_json(client.request(req).await?).await
}

pub(crate) async fn get_json<R: DeserializeOwned>(client: &HttpClient, uri: Uri) -> Result<R> {
	let req = Request::get(uri).body(Body::empty())?;
	read_json(client.request(req).await?).await
}

/// Forwards `req` to `target`. Scheme and host come from `target`,
/// path and query from the original request.
pub(crate) async fn forward(client: &HttpClient, target: Uri, mut req: Request) -> Result<Response> {
	let mut parts = target.into_parts();
	parts.path_and_query = Some(
		req.uri().path_and_query().cloned()
			.unwrap_or_else(|| PathAndQuery::from_static("/")),
	);
	*req.uri_mut() = Uri::from_parts(parts)?;

	req.headers_mut().insert("x-jdb-hop", HeaderValue::from_static("1"));
	req.headers_mut().remove(header::CONNECTION);

	Ok(client.request(req).await?.map(Body::new))
}

async fn read_json<R: DeserializeOwned>(res: hyper::Response<Incoming>) -> Result<R> {
	let status = res.status();
	let bytes = res.into_body().collect().await?.to_bytes();

	if !status.is_success() {
		return Err(format!("{status}: {}", String::from_utf8_lossy(&bytes)).into());
	}
	Ok(serde_json::from_slice(&bytes)?)
}


pub(crate) fn now() -> String {
	Local::now().format("%Y-%m-%d %H-%M-%S").to_string()
}

pub(crate) fn parse_duration(s: &str) -> std::result::Result<Duration, String> {
	let s = s.trim();
	let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
	let (num, unit) = s.split_at(split);

	let n: u64 = num.parse().map_err(|_| format!("invalid number in '{s}'"))?;
	let mul = |f: u64| n.checked_mul(f).ok_or_else(|| format!("'{s}' is too large"));

	Ok(match unit.trim() {
		"µs" | "us"       => Duration::from_micros(n),
		"ms"              => Duration::from_millis(n),
		"s" | ""          => Duration::from_secs(n),
		"m" | "min"       => Duration::from_secs(mul(60)?),
		"h"               => Duration::from_secs(mul(3600)?),
		u => return Err(format!("unknown unit '{u}' (µs, ms, s, m, h)")),
	})
}

pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
	if a.len() != b.len() {
		return false;
	}
	let mut diff = 0u8;
	for (x, y) in a.iter().zip(b) {
		diff |= x ^ y;
	}
	std::hint::black_box(diff) == 0
}