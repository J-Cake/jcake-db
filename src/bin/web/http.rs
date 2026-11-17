use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use axum_core::response::IntoResponse;
use serde_json::Value;
use uuid::Uuid;
use crate::local_db::DbHandle;

pub(crate) mod method {
	use axum::extract::Request;
	use axum::extract::State;
	use axum::handler::Handler;
	use axum::http::StatusCode;
	use axum::response::IntoResponse;
	pub use axum::routing::*;

	pub fn query<H, T, S>(handler: H) -> MethodRouter<S>
	where
		H: Handler<T, S>,
		T: 'static,
		S: Clone + Send + Sync + 'static, {
		MethodRouter::new().fallback(move |State(state): State<S>, req: Request| async move {
			if req.method().as_str() == "QUERY" {
				handler.call(req, state).await
			} else {
				StatusCode::METHOD_NOT_ALLOWED.into_response()
			}
		})
	}
}
#[axum::debug_handler]
pub(crate) async fn query(db: State<DbHandle>, body: Json<String>) -> impl IntoResponse {
	match db.query(body.0).await {
		Ok(data) => (StatusCode::OK, Json(data)).into_response(),
		Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
	}
}

#[axum::debug_handler]
pub(crate) async fn fetch(path: Path<(Uuid,)>, db: State<DbHandle>) -> impl IntoResponse {
	match db.fetch(path.0.0).await {
		Ok(Some(obj)) => (StatusCode::OK, Json(obj)).into_response(),
		Ok(None) => (
			StatusCode::NOT_FOUND,
			Json(format!("No object {uuid}", uuid = path.0.0)),
		)
			.into_response(),
		Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
	}
}

#[axum::debug_handler]
pub(crate) async fn insert(db: State<DbHandle>, body: Json<Value>) -> impl IntoResponse {
	match db.insert(body.0).await {
		Ok(uuid) => (StatusCode::OK, Json(uuid)).into_response(),
		Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
	}
}

#[axum::debug_handler]
pub(crate) async fn upsert(path: Path<(Uuid,)>, db: State<DbHandle>, body: Json<Value>) -> impl IntoResponse {
	match db.upsert(path.0.0, body.0).await {
		Ok(()) => (StatusCode::OK, Json(())).into_response(),
		Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
	}
}

#[axum::debug_handler]
pub(crate) async fn delete(path: Path<(Uuid,)>, db: State<DbHandle>) -> impl IntoResponse {
	match db.delete(path.0.0).await {
		Ok(()) => (StatusCode::OK, Json(())).into_response(),
		Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
	}
}