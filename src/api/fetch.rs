use crate::api::auth;
use crate::error::AppError;
use crate::pool::manager::{
    deserialize_opt_f64, deserialize_opt_usize, ProxyFilter, ProxyListQuery,
};
use crate::AppState;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct FetchQuery {
    pub api_key: Option<String>,
    #[serde(default)]
    pub chatgpt: bool,
    #[serde(default)]
    pub google: bool,
    #[serde(default)]
    pub residential: bool,
    #[serde(default, deserialize_with = "deserialize_opt_f64")]
    pub risk_max: Option<f64>,
    pub country: Option<String>,
    #[serde(rename = "type")]
    pub proxy_type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_usize")]
    pub count: Option<usize>,
    pub proxy_id: Option<String>,
}

pub async fn fetch_proxies(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<FetchQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth::authenticate_request(&state, &headers, query.api_key.as_deref()).await?;

    let filter = ProxyFilter {
        chatgpt: query.chatgpt,
        google: query.google,
        residential: query.residential,
        risk_max: query.risk_max,
        country: query.country,
        proxy_type: query.proxy_type,
        count: query.count,
        proxy_id: query.proxy_id,
    };
    let count = filter.count.unwrap_or(1);

    if let Some(ref id) = filter.proxy_id {
        if let Some(proxy) = state.pool.get(id) {
            return Ok(Json(json!({
                "proxies": [proxy_to_json(&proxy)]
            })));
        } else {
            return Err(AppError::NotFound(format!("Proxy {id} not found")));
        }
    }

    let proxies = state.pool.pick_random(&filter, count);
    if proxies.is_empty() {
        return Ok(Json(json!({
            "proxies": [],
            "message": "No proxies match the given filters"
        })));
    }

    let proxy_list: Vec<serde_json::Value> = proxies.iter().map(proxy_to_json).collect();

    Ok(Json(json!({
        "proxies": proxy_list,
        "count": proxy_list.len(),
    })))
}

/// User-accessible proxy list with full quality details
pub async fn list_all_proxies(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<UserProxyListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth::authenticate_request(&state, &headers, query.api_key.as_deref()).await?;

    let list_query = ProxyListQuery {
        page: query.page,
        per_page: query.per_page,
        search: query.search,
        status: query.status,
        proxy_type: query.proxy_type,
        quality: query.quality,
        sort: query.sort,
        dir: query.dir,
    };
    let stats = state.pool.stats();
    let result = state.pool.list_proxies(&list_query);
    let proxy_list: Vec<serde_json::Value> = result.proxies.iter().map(proxy_to_json).collect();

    Ok(Json(json!({
        "proxies": proxy_list,
        "total": stats.total,
        "filtered_total": result.total,
        "page": result.page,
        "per_page": result.per_page,
        "valid": stats.valid,
        "untested": stats.untested,
        "invalid": stats.invalid,
        "quality_checked": stats.quality_checked,
        "chatgpt_accessible": stats.chatgpt_accessible,
        "google_accessible": stats.google_accessible,
        "residential": stats.residential,
    })))
}

#[derive(Debug, Deserialize)]
pub struct UserProxyListQuery {
    pub api_key: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_usize")]
    pub page: Option<usize>,
    #[serde(default, deserialize_with = "deserialize_opt_usize")]
    pub per_page: Option<usize>,
    pub search: Option<String>,
    pub status: Option<String>,
    #[serde(rename = "type")]
    pub proxy_type: Option<String>,
    pub quality: Option<String>,
    pub sort: Option<String>,
    pub dir: Option<String>,
}

fn proxy_to_json(p: &crate::pool::manager::PoolProxy) -> serde_json::Value {
    json!({
        "id": p.id,
        "name": p.name,
        "type": p.proxy_type,
        "server": p.server,
        "port": p.port,
        "local_port": p.local_port,
        "status": p.status,
        "error_count": p.error_count,
        "quality": p.quality.as_ref().map(|q| json!({
            "ip_address": q.ip_address,
            "country": q.country,
            "ip_type": q.ip_type,
            "is_residential": q.is_residential,
            "chatgpt": q.chatgpt_accessible,
            "google": q.google_accessible,
            "risk_score": q.risk_score,
            "risk_level": q.risk_level,
        })),
    })
}
