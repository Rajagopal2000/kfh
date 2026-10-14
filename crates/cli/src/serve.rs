//! `kfh serve`: JSON API plus the embedded single-page UI.

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use history::query;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE_CSS: &str = include_str!("../web/style.css");

struct AppState {
    db_path: PathBuf,
    versions: Vec<String>,
}

pub fn serve(db_path: PathBuf, addr: &str) -> Result<()> {
    let versions = query::versions(&open(&db_path)?)?.into_iter().map(|v| v.tag).collect();
    let state = Arc::new(AppState { db_path, versions });
    let app = Router::new()
        .route("/", get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }))
        .route(
            "/f/{name}",
            get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }),
        )
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }),
        )
        .route(
            "/style.css",
            get(|| async { asset("text/css; charset=utf-8", STYLE_CSS) }),
        )
        .route("/healthz", get(healthz))
        .route("/api/search", get(search))
        .route("/api/function/{name}", get(function))
        .with_state(state);

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        eprintln!("serving on http://{}", listener.local_addr()?);
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        Ok(())
    })
}

fn open(path: &PathBuf) -> Result<Connection> {
    Ok(Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?)
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(e.into())
    }
}

/// Runs a blocking database query off the async executor.
async fn with_db<T: Send + 'static>(
    state: &Arc<AppState>,
    f: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    let path = state.db_path.clone();
    Ok(tokio::task::spawn_blocking(move || f(&open(&path)?)).await??)
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
}

async fn search(
    State(state): State<Arc<AppState>>,
    Query(p): Query<SearchParams>,
) -> Result<Json<Vec<String>>, ApiError> {
    let q = p.q.trim().to_string();
    if q.is_empty() {
        return Ok(Json(Vec::new()));
    }
    Ok(Json(with_db(&state, move |db| query::search(db, &q, 20)).await?))
}

#[derive(Serialize)]
struct FunctionResponse {
    name: String,
    versions: Vec<String>,
    defs: Vec<query::DefHistory>,
}

async fn function(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<FunctionResponse>, ApiError> {
    let lookup = name.clone();
    let defs = with_db(&state, move |db| query::history(db, &lookup)).await?;
    Ok(Json(FunctionResponse {
        name,
        versions: state.versions.clone(),
        defs,
    }))
}

/// Ready when the database answers a query.
async fn healthz(State(state): State<Arc<AppState>>) -> Result<&'static str, ApiError> {
    with_db(&state, |db| {
        Ok(db.query_row("SELECT 1 FROM versions LIMIT 1", [], |_| Ok(()))?)
    })
    .await?;
    Ok("ok")
}

/// Resolves on SIGINT or SIGTERM, so in-flight requests finish before exit.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    eprintln!("shutting down");
}
