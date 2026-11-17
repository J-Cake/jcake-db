use std::time::Duration;

use crate::util::colours::ColouredStringBuilder;
use chrono::Local;
use format::Display;
use format::Error;

pub(crate) mod http {
	use libc::option;
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
	use hyper_util::client::legacy::Client;
	use hyper_util::client::legacy::connect::HttpConnector;
	use serde::Serialize;
	use serde::de::DeserializeOwned;

	#[macro_export]
	macro_rules! headers {
		($($header:literal: $val:literal),*) => {
			try {
				let mut headers = HeaderMap::new();
				$(headers.insert($header, HeaderValue::from_str($val)?);)*
				headers
			}
		}
	}

	pub(crate) struct HttpClient {
		client: Client<HttpConnector, Body>,
		blanket_headers: HeaderMap
	}

	pub(crate) struct RequestArgs<Body: BodyExt> {
		pub(crate) method: Method,
		pub(crate) uri: Uri,
		pub(crate) headers: HeaderMap,
		pub(crate) body: Option<Body>
	}

	#[derive(Debug, Display, Error)]
	pub(crate) enum RequestError {
		#[from_err]
		HttpError(hyper::http::Error),

		#[from_err]
		RequestError(hyper_util::client::legacy::Error),

		#[from_err]
		AxumError(axum::Error),

		#[from_err]
		ContentError(ResponseHandle),

		#[from_err]
		Utf8Error(std::str::Utf8Error),

		#[from_err]
		DeserialisationError(super::DeserialisationError),


		#[from_err]
		Miscellaneous(String),
	}

	#[derive(Debug)]
	pub(crate) struct ResponseHandle<Body: http_body_util::BodyExt = axum::body::Body> {
		status: StatusCode,
		body: Body,
		headers: HeaderMap
	}

	impl HttpClient {
		pub(crate) async fn req(&self, req: RequestArgs<Body>) -> Result<ResponseHandle, RequestError> {
			let mut builder = Request::builder()
				.method(req.method)
				.uri(req.uri);

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

			if let Some(body) = req.body {
				let res = self.client.request(builder.body(body)?).await?;

				Ok(ResponseHandle {
					status: res.status(),
					headers: res.headers().clone(),
					body: res.body().into(),
				})
			} else {
				let res = self.client.request(builder.body(Body::empty())?).await?;

				Ok(ResponseHandle {
					status: res.status(),
					headers: res.headers().clone(),
					body: res.body().into(),
				})
			}
		}

		async fn handle_json_body<Res: DeserializeOwned>(&self, res: ResponseHandle) -> Result<Res, RequestError> {
			if !(200u16..300).contains(&res.status.as_u16()) {
				return Err(res.into())
			}

			match res.headers.get("Content-Type").map(|val| val.to_str()) {
				Some(Ok("application/json")) => {
					let body = res.body.collect().await?
						.to_bytes()
						.to_vec();

					Ok(serde_json::from_slice(&body).map_err(Into::into)?)
				},
				Some(Ok("application/jsonb")) => {
					let body = res.body.collect().await?
						.to_bytes()
						.to_vec();

					let raw = jsonb::RawJsonb::from(body);
					Ok(jsonb::from_raw_jsonb(&raw).map_err(Into::into)??)
				}
				_ => Err("Response is not in JSON".to_string().into())
			}
		}

		pub(crate) async fn get_json<Res: DeserializeOwned>(&self, uri: Uri) -> Result<Res, RequestError> {
			let res = self.req(RequestArgs {
				uri,
				method: Method::GET,
				headers: headers! {
					"Accept": "application/json, application/jsonb",
					"Accept-Encoding": "utf-8"
				}?,
				body: None,
			}).await?;

			self.handle_json_body(res)
		}

		pub(crate) async fn post_json<Req: Serialize, Res: DeserializeOwned>(&self, uri: Uri, req: Req) -> Result<Res, RequestError> {
			let body = Body::new(serde_json::to_string(&req)
				.map_err(Into::into)?);

			let res = self.req(RequestArgs {
				uri,
				method: Method::POST,
				headers: headers! {
					"Content-Type": "application/json",
					"Accept": "application/json, application/jsonb",
					"Accept-Encoding": "utf-8"
				}?,
				body: Some(body)
			}).await?;

			self.handle_json_body(res)
		}

		pub(crate) async fn forward(&self, req: Request<Body>, to: Uri) -> Result<(), RequestError> {
			self.req(RequestArgs {
				uri: to,
				method: req.method().clone(),
				headers: req.headers().clone(),
				body: Some(req.body().into_stream().into())
			})
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
			self.header(header::CONTENT_LENGTH, HeaderValue::from_str(&header_value.to_string())
				.expect("Infallible"))
		}

		pub(crate) fn body(mut self, body: Body) -> Self {
			self.body.replace(body);
			self
		}
	}
}

#[derive(Debug, Display, Error)]
pub(crate) enum DeserialisationError {
	#[from_err]
	Json(serde_json::Error),

	#[from_err]
	Jsonb(jsonb::Error)
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

pub mod colours {
	use crate::util::now;
	use std::ops::{Deref, DerefMut};

	const RESET: &str = "\x1b[m";

	macro_rules! colours {
    ($(const $name:ident = $colour:literal;)*) => {
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

		pub fn now(mut self) -> Self {
			self.grey(format!("[{now}] ", now=now()))
		}

		pub fn finish(self) -> String {
			self.str
		}

		pub fn print(self) {
			println!("{str}", str=self.str);
		}

		pub fn eprint(self) {
			eprintln!("{str}", str=self.str);
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
	ColouredStringBuilder::default()
		.now()
		.normal(msg)
		.print();
}

pub(crate) fn error(msg: impl AsRef<str>) {
	ColouredStringBuilder::default()
		.now()
		.red("Error")
		.normal(msg)
		.eprint();
}