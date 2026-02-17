mod config;
mod discovery;

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use discovery::{Instance, InstanceRegistry, Registry};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
struct AppState {
    registry: Registry,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "blacksmith_ui=info".parse().unwrap()),
        )
        .init();

    let cwd = std::env::current_dir().unwrap_or_default();
    let cfg = config::load_config(&cwd);

    // Build registry with manual + runtime-persisted entries
    let mut registry = InstanceRegistry::new();
    registry.add_manual_entries(&cfg.projects);

    let runtime_instances = config::load_runtime_instances();
    for ri in &runtime_instances {
        registry.add_runtime(&ri.url, &ri.name);
    }

    let registry = Arc::new(RwLock::new(registry));

    // Spawn UDP listener and sweep task
    discovery::spawn_udp_listener(Arc::clone(&registry));
    discovery::spawn_sweep_task(Arc::clone(&registry));

    let state = AppState { registry };

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/instances", get(list_instances))
        .route("/api/instances", post(add_instance))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("{}:{}", cfg.dashboard.bind, cfg.dashboard.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    tracing::info!("blacksmith-ui listening on {local_addr}");

    axum::serve(listener, app).await.unwrap();
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true}))
}

async fn list_instances(State(state): State<AppState>) -> Json<Vec<Instance>> {
    let reg = state.registry.read().await;
    Json(reg.list())
}

#[derive(Deserialize)]
struct AddInstanceRequest {
    url: String,
    #[serde(default)]
    name: Option<String>,
}

async fn add_instance(
    State(state): State<AppState>,
    Json(req): Json<AddInstanceRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let url = req.url.trim_end_matches('/').to_string();

    // Probe health endpoint
    let health_url = format!("{url}/api/health");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("http client error: {e}")})),
            )
        })?;

    let resp = client.get(&health_url).send().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("health check failed: {e}")})),
        )
    })?;

    if !resp.status().is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("health check returned {}", resp.status())})),
        ));
    }

    let name = req.name.unwrap_or_else(|| {
        // Try to extract name from URL
        url.split("://")
            .nth(1)
            .unwrap_or(&url)
            .split(':')
            .next()
            .unwrap_or("unknown")
            .to_string()
    });

    let mut reg = state.registry.write().await;
    reg.add_runtime(&url, &name);

    Ok(Json(
        serde_json::json!({"ok": true, "url": url, "name": name}),
    ))
}
