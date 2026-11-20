use crate::util::colours::ColouredStringBuilder;
use chrono::Local;
use format::Display;
use format::Error;
use std::time::Duration;

pub(crate) mod http {
	use crate::util::DeserialisationError;
	use axum::http::HeaderName;
	use axum::http::HeaderValue;
	use axum::http::Method;
	use axum::http::header;
	use axum_core::body::Body;
	use format::Display;
	use format::Error;
	use http_body_util::BodyExt;
	use hyper::HeaderMap;
	use hyper::Request;
	use hyper::StatusCode;
	use hyper::Uri;
	use hyper::header::InvalidHeaderValue;
	use hyper_util::client::legacy::Client;
	use hyper_util::client::legacy::connect::HttpConnector;
	use hyper_util::rt::TokioExecutor;
	use libc::option;
	use serde::Serialize;
	use serde::de::DeserializeOwned;
	use std::string::FromUtf8Error;
	use std::time::Duration;

	#[macro_export]
	macro_rules! headers {
		($($header:literal: $val:expr),*) => {
			try {
				let mut headers = HeaderMap::new();
				$(headers.insert($header, HeaderValue::from_str($val)?);)*
				headers
			}
		}
	}

	pub(crate) struct HttpClient {
		client: Client<HttpConnector, Body>,
		blanket_headers: HeaderMap,
	}

	#[derive(Default)]
	pub(crate) struct RequestArgs<Body: BodyExt> {
		pub(crate) method: Method,
		pub(crate) uri: Uri,
		pub(crate) headers: HeaderMap,
		pub(crate) body: Option<Body>,
	}

	#[derive(Debug, Display, Error)]
	pub(crate) enum RequestError {
		#[from_err]
		HttpError(hyper::http::Error),

		#[from_err]
		RequestError(hyper_util::client::legacy::Error),

		#[from_err]
		AxumError(axum::Error),

		#[from_err(no_source)]
		ContentError(ResponseHandle),

		#[from_err]
		Utf8Error(std::str::Utf8Error),

		#[from_err]
		FromUtf8Error(FromUtf8Error),

		#[from_err]
		DeserialisationError(super::DeserialisationError),

		#[from_err]
		InvalidHeaderError(InvalidHeaderValue),

		#[from_err(no_source)]
		Miscellaneous(String),
	}

	#[derive(Debug)]
	pub(crate) struct ResponseHandle<Body: http_body_util::BodyExt = axum::body::Body> {
		status: StatusCode,
		body: Body,
		headers: HeaderMap,
	}

	impl HttpClient {
		pub(crate) fn new() -> Self {
			Self {
				client: Client::builder(TokioExecutor::new())
					.pool_idle_timeout(Duration::from_secs(90))
					.build_http(),
				blanket_headers: Default::default(),
			}
		}

		pub(crate) fn with_headers(mut self, headers: HeaderMap) -> Self {
			self.blanket_headers = headers;
			self
		}

		pub(crate) async fn req(&self, req: RequestArgs<Body>) -> Result<ResponseHandle, RequestError> {
			let mut builder = Request::builder().method(req.method).uri(req.uri);

			if let Some(headers) = builder.headers_mut() {
				*headers = req.headers;
			} else {
				for (name, value) in req.headers.iter() {
					builder = builder.header(name, value);
				}
			}

			for (name, value) in &self.blanket_headers {
				builder = builder.header(name, value);
			}

			let body = req.body.unwrap_or_else(Body::empty);
			let (res, incoming) = self.client.request(builder.body(body)?).await?.into_parts();

			Ok(ResponseHandle {
				status: res.status,
				headers: res.headers,
				body: Body::new(incoming),
			})
		}

		async fn handle_json_body<Res: DeserializeOwned>(&self, res: ResponseHandle) -> Result<Res, RequestError> {
			res.into_json().await
		}

		pub(crate) async fn get_json<Res: DeserializeOwned>(&self, uri: Uri) -> Result<Res, RequestError> {
			let res = self
				.req(RequestArgs {
					uri,
					method: Method::GET,
					headers: headers! {
						"Accept": "application/json, application/jsonb",
						"Accept-Encoding": "utf-8"
					}?,
					body: None,
				})
				.await?;

			self.handle_json_body(res).await
		}

		pub(crate) async fn post_json<Req: Serialize, Res: DeserializeOwned>(&self, uri: Uri, req: Req) -> Result<Res, RequestError> {
			let body = Body::new(serde_json::to_string(&req).map_err(DeserialisationError::from)?);

			let res = self
				.req(RequestArgs {
					uri,
					method: Method::POST,
					headers: headers! {
						"Content-Type": "application/json",
						"Accept": "application/json, application/jsonb",
						"Accept-Encoding": "utf-8"
					}?,
					body: Some(body),
				})
				.await?;

			self.handle_json_body(res).await
		}

		pub(crate) async fn forward(&self, req: Request<Body>, to: Uri) -> Result<(), RequestError> {
			let (req, body) = req.into_parts();

			self.req(RequestArgs {
				uri: to,
				method: req.method,
				headers: req.headers,
				body: Some(Body::new(body)),
			})
			.await?;

			Ok(())
		}
	}

	impl<Body: BodyExt> RequestArgs<Body> {
		pub(crate) fn new(method: Method, uri: Uri) -> Self {
			Self {
				method,
				uri,
				headers: Default::default(),
				body: None,
			}
		}

		pub(crate) fn list_header(mut self, header_name: HeaderName, header_value: HeaderValue) -> Self {
			self.headers.append(header_name, header_value);
			self
		}

		pub(crate) fn header(mut self, header_name: HeaderName, header_value: HeaderValue) -> Self {
			self.headers.insert(header_name, header_value);
			self
		}

		pub(crate) fn sensitive_header(mut self, header_name: HeaderName, mut header_value: HeaderValue) -> Self {
			header_value.set_sensitive(true);
			self.headers.insert(header_name, header_value);
			self
		}

		pub(crate) fn authorization(self, header_value: HeaderValue) -> Self {
			self.sensitive_header(header::AUTHORIZATION, header_value)
		}

		pub(crate) fn content_type(self, header_value: HeaderValue) -> Self {
			self.header(header::CONTENT_TYPE, header_value)
		}

		pub(crate) fn content_length(self, header_value: usize) -> Self {
			self.header(header::CONTENT_LENGTH, HeaderValue::from_str(&header_value.to_string()).expect("Infallible"))
		}

		pub(crate) fn body(mut self, body: Body) -> Self {
			self.body.replace(body);
			self
		}
	}

	impl ResponseHandle {
		fn into_body(self) -> Body {
			self.body
		}

		pub(crate) async fn into_json<De: DeserializeOwned>(self) -> Result<De, RequestError> {
			if !(200u16..300).contains(&self.status.as_u16()) {
				let text = self.into_text().await?;
				return Err(text.into());
			}

			match self.headers.get("Content-Type").map(|val| val.to_str()) {
				Some(Ok("application/json")) => {
					let body = self.body.collect().await?.to_bytes().to_vec();

					Ok(serde_json::from_slice(&body).map_err(DeserialisationError::from)?)
				}
				Some(Ok("application/jsonb")) => {
					let body = self.body.collect().await?.to_bytes().to_vec();

					let raw = jsonb::RawJsonb::new(&body);
					Ok(jsonb::from_raw_jsonb(&raw).map_err(DeserialisationError::from)?)
				}
				_ => Err("Response is not in JSON".to_string().into()),
			}
		}

		pub(crate) async fn into_text(self) -> Result<String, RequestError> {
			let body = self.body.collect().await?.to_bytes().to_vec();
			Ok(String::from_utf8(body)?)
		}

		pub(crate) fn is_informational(&self) -> bool {
			self.status.is_informational()
		}

		pub(crate) fn is_success(&self) -> bool {
			self.status.is_success()
		}

		pub(crate) fn is_redirection(&self) -> bool {
			self.status.is_redirection()
		}

		pub(crate) fn is_client_error(&self) -> bool {
			self.status.is_client_error()
		}

		pub(crate) fn is_server_error(&self) -> bool {
			self.status.is_server_error()
		}
	}
}

#[derive(Debug, Display, Error)]
pub(crate) enum DeserialisationError {
	#[from_err]
	Json(serde_json::Error),

	#[from_err]
	Jsonb(jsonb::Error),
}

pub(crate) fn now() -> String {
	Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub(crate) fn parse_duration(s: &str) -> std::result::Result<Duration, String> {
	let s = s.trim();
	let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
	let (num, unit) = s.split_at(split);

	let n: u64 = num.parse().map_err(|_| format!("invalid number in '{s}'"))?;
	let mul = |f: u64| n.checked_mul(f).ok_or_else(|| format!("'{s}' is too large"));

	Ok(match unit.trim() {
		"µs" | "us" => Duration::from_micros(n),
		"ms" => Duration::from_millis(n),
		"s" | "" => Duration::from_secs(n),
		"m" | "min" => Duration::from_secs(mul(60)?),
		"h" => Duration::from_secs(mul(3600)?),
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

pub mod colours {
	use crate::util::now;
	use std::fmt::Debug;
	use std::fmt::Formatter;
	use std::fmt::FormattingOptions;
	use std::ops::Deref;
	use std::ops::DerefMut;

	const RESET: &str = "\x1b[m";

	macro_rules! colours {
	($(const $name:ident = $colour:literal;)*) => {
		#[allow(unused_variables)]
		$(
			pub fn $name(mut self, text: impl AsRef<str>) -> Self {
				self.str.push_str($colour);
				self.str.push_str(text.as_ref());
				self.str.push_str(RESET);
				self
			}
		)*
	};
}

	#[derive(Default)]
	pub struct ColouredStringBuilder {
		str: String,
	}

	impl ColouredStringBuilder {
		colours! {
			const black = "\x1b[30m";
			const red = "\x1b[31m";
			const green = "\x1b[32m";
			const yellow = "\x1b[33m";
			const blue = "\x1b[34m";
			const purple = "\x1b[35m";
			const cyan = "\x1b[36m";
			const white = "\x1b[37m";
			const grey = "\x1b[90m";
			const red_intense = "\x1b[91m";
			const green_intense = "\x1b[92m";
			const yellow_intense = "\x1b[93m";
			const blue_intense = "\x1b[94m";
			const purple_intense = "\x1b[95m";
			const cyan_intense = "\x1b[96m";
			const white_intense = "\x1b[97m";
		}

		pub fn normal(mut self, text: impl AsRef<str>) -> Self {
			self.str.push_str(text.as_ref());
			self
		}

		pub fn dbg(self, text: impl Debug) -> Self {
			let mut str = String::new();
			let mut fmt = FormattingOptions::default().create_formatter(&mut str);

			text.fmt(&mut fmt).expect("Debug logging failed");

			self.normal(str)
		}

		pub fn now(mut self) -> Self {
			self.grey(format!("[{now}] ", now = now()))
		}

		pub fn finish(self) -> String {
			self.str
		}

		pub fn print(self) {
			println!("{str}", str = self.str);
		}

		pub fn eprint(self) {
			eprintln!("{str}", str = self.str);
		}
	}

	impl Deref for ColouredStringBuilder {
		type Target = String;

		fn deref(&self) -> &Self::Target {
			&self.str
		}
	}

	impl DerefMut for ColouredStringBuilder {
		fn deref_mut(&mut self) -> &mut Self::Target {
			&mut self.str
		}
	}
}

pub(crate) fn info(msg: impl AsRef<str>) {
	ColouredStringBuilder::default().now().normal(msg).print();
}

pub(crate) fn warning(msg: impl AsRef<str>) {
	ColouredStringBuilder::default().now().yellow("Warning: ").normal(msg).eprint();
}

pub(crate) fn error(msg: impl AsRef<str>) {
	ColouredStringBuilder::default().now().red("Error: ").normal(msg).eprint();
}

pub(crate) trait MaxN: Iterator + Sized {
	/// The `n` largest items by key, largest first. On ties the later item
	/// ranks first, matching `max_by_key`.
	fn max_n_by_key<K: Ord>(
		self, n: usize, mut key: impl FnMut(&Self::Item) -> K,
	) -> Vec<Self::Item> {
		let mut top: Vec<(K, Self::Item)> = Vec::with_capacity(n + 1);

		if n == 0 {
			return Vec::new();
		}

		for item in self {
			let k = key(&item);

			// first position whose key isn't strictly greater: new item goes before equals
			let pos = top.partition_point(|(tk, _)| *tk > k);

			if pos < n {
				top.insert(pos, (k, item));
				top.truncate(n);
			}
		}

		top.into_iter().map(|(_, item)| item).collect()
	}
}

impl<I: Iterator> MaxN for I {}
