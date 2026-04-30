use crate::db::{Database, ProxyQuality};
use dashmap::DashMap;
use rand::Rng;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyStatus {
    Untested,
    Valid,
    Invalid,
}

impl ProxyStatus {
    /// Sort weight: Valid=0, Untested=1, Invalid=2 (lower = higher priority).
    pub fn sort_weight(self) -> u8 {
        match self {
            ProxyStatus::Valid => 0,
            ProxyStatus::Untested => 1,
            ProxyStatus::Invalid => 2,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PoolProxy {
    pub id: String,
    pub subscription_id: String,
    pub name: String,
    pub proxy_type: String,
    pub server: String,
    pub port: u16,
    pub singbox_outbound: serde_json::Value,
    pub status: ProxyStatus,
    pub local_port: Option<u16>,
    pub error_count: u32,
    pub quality: Option<ProxyQualityInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyQualityInfo {
    pub ip_address: Option<String>,
    pub country: Option<String>,
    pub ip_type: Option<String>,
    pub is_residential: bool,
    pub chatgpt_accessible: bool,
    pub google_accessible: bool,
    pub risk_score: f64,
    pub risk_level: String,
    pub checked_at: Option<String>,
    #[serde(skip_serializing)]
    pub incomplete_retry_count: u8,
}

impl From<ProxyQuality> for ProxyQualityInfo {
    fn from(q: ProxyQuality) -> Self {
        let incomplete_retry_count = q
            .extra_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v.get("incomplete_retry_count").and_then(|n| n.as_u64()))
            .map(|n| n.min(u8::MAX as u64) as u8)
            .unwrap_or(0);

        ProxyQualityInfo {
            ip_address: q.ip_address,
            country: q.country,
            ip_type: q.ip_type,
            is_residential: q.is_residential,
            chatgpt_accessible: q.chatgpt_accessible,
            google_accessible: q.google_accessible,
            risk_score: q.risk_score,
            risk_level: q.risk_level,
            checked_at: Some(q.checked_at),
            incomplete_retry_count,
        }
    }
}

pub struct ProxyPool {
    proxies: DashMap<String, PoolProxy>,
}

impl ProxyPool {
    pub fn new() -> Self {
        ProxyPool {
            proxies: DashMap::new(),
        }
    }

    pub fn load_from_db(&self, db: &Database) {
        let rows = db.get_all_proxies().unwrap_or_default();
        let qualities = db.get_all_qualities().unwrap_or_default();
        let quality_map: std::collections::HashMap<String, ProxyQuality> = qualities
            .into_iter()
            .map(|q| (q.proxy_id.clone(), q))
            .collect();

        for row in rows {
            let quality = quality_map
                .get(&row.id)
                .map(|q| ProxyQualityInfo::from(q.clone()));
            let outbound: serde_json::Value =
                serde_json::from_str(&row.config_json).unwrap_or_default();
            // Derive tri-state: never validated → Untested, validated ok → Valid, validated fail → Invalid
            let status = if row.is_valid {
                ProxyStatus::Valid
            } else if row.last_validated.is_some() {
                ProxyStatus::Invalid
            } else {
                ProxyStatus::Untested
            };
            let proxy = PoolProxy {
                id: row.id.clone(),
                subscription_id: row.subscription_id,
                name: row.name,
                proxy_type: row.proxy_type,
                server: row.server,
                port: row.port as u16,
                singbox_outbound: outbound,
                status,
                local_port: row.local_port.map(|p| p as u16),
                error_count: row.error_count as u32,
                quality,
            };
            self.proxies.insert(row.id, proxy);
        }
        tracing::info!("Loaded {} proxies into pool", self.proxies.len());
    }

    pub fn add(&self, proxy: PoolProxy) {
        self.proxies.insert(proxy.id.clone(), proxy);
    }

    pub fn remove(&self, id: &str) {
        self.proxies.remove(id);
    }

    pub fn get(&self, id: &str) -> Option<PoolProxy> {
        self.proxies.get(id).map(|p| p.clone())
    }

    pub fn get_all(&self) -> Vec<PoolProxy> {
        self.proxies.iter().map(|p| p.value().clone()).collect()
    }

    pub fn get_valid_proxies(&self) -> Vec<PoolProxy> {
        self.proxies
            .iter()
            .filter(|p| p.status == ProxyStatus::Valid)
            .map(|p| p.value().clone())
            .collect()
    }

    pub fn set_status(&self, id: &str, status: ProxyStatus) {
        if let Some(mut proxy) = self.proxies.get_mut(id) {
            proxy.status = status;
            match status {
                ProxyStatus::Valid => proxy.error_count = 0,
                ProxyStatus::Invalid => proxy.error_count += 1,
                ProxyStatus::Untested => {}
            }
        }
    }

    pub fn set_local_port(&self, id: &str, port: u16) {
        if let Some(mut proxy) = self.proxies.get_mut(id) {
            proxy.local_port = Some(port);
        }
    }

    pub fn clear_local_port(&self, id: &str) {
        if let Some(mut proxy) = self.proxies.get_mut(id) {
            proxy.local_port = None;
        }
    }

    pub fn clear_all_local_ports(&self) {
        for mut proxy in self.proxies.iter_mut() {
            proxy.local_port = None;
        }
    }

    pub fn set_quality(&self, id: &str, quality: ProxyQualityInfo) {
        if let Some(mut proxy) = self.proxies.get_mut(id) {
            proxy.quality = Some(quality);
        }
    }

    pub fn count(&self) -> usize {
        self.proxies.len()
    }

    pub fn count_valid(&self) -> usize {
        self.proxies
            .iter()
            .filter(|p| p.status == ProxyStatus::Valid)
            .count()
    }

    pub fn remove_by_subscription(&self, sub_id: &str) {
        let ids: Vec<String> = self
            .proxies
            .iter()
            .filter(|p| p.subscription_id == sub_id)
            .map(|p| p.id.clone())
            .collect();
        for id in ids {
            self.proxies.remove(&id);
        }
    }

    pub fn update_proxy_config(&self, id: &str, name: &str, singbox_outbound: serde_json::Value) {
        if let Some(mut proxy) = self.proxies.get_mut(id) {
            proxy.name = name.to_string();
            proxy.singbox_outbound = singbox_outbound;
        }
    }

    pub fn increment_error(&self, id: &str) {
        if let Some(mut proxy) = self.proxies.get_mut(id) {
            proxy.error_count += 1;
        }
    }

    pub fn filter_proxies(&self, filter: &ProxyFilter) -> Vec<PoolProxy> {
        self.proxies
            .iter()
            .filter(|p| proxy_matches_filter(p.value(), filter))
            .map(|p| p.value().clone())
            .collect()
    }

    pub fn pick_random(&self, filter: &ProxyFilter, count: usize) -> Vec<PoolProxy> {
        if count == 0 {
            return Vec::new();
        }

        let mut rng = rand::thread_rng();
        let mut reservoir: Vec<PoolProxy> = Vec::with_capacity(count);
        let mut seen = 0usize;

        for entry in self.proxies.iter() {
            let proxy = entry.value();
            if !proxy_matches_filter(proxy, filter) {
                continue;
            }

            seen += 1;
            if reservoir.len() < count {
                reservoir.push(proxy.clone());
                continue;
            }

            let idx = rng.gen_range(0..seen);
            if idx < count {
                reservoir[idx] = proxy.clone();
            }
        }

        reservoir
    }

    pub fn list_proxies(&self, query: &ProxyListQuery) -> ProxyListResult {
        let mut matched: Vec<PoolProxy> = self
            .proxies
            .iter()
            .filter(|p| proxy_matches_list_query(p.value(), query))
            .map(|p| p.value().clone())
            .collect();

        sort_proxies(&mut matched, query.sort.as_deref(), query.dir.as_deref());

        let total = matched.len();
        let page = query.page.unwrap_or(1).max(1);
        let per_page = query.per_page.unwrap_or(50).clamp(1, 500);
        let start = (page - 1).saturating_mul(per_page);
        let proxies = if start >= total {
            Vec::new()
        } else {
            matched.into_iter().skip(start).take(per_page).collect()
        };

        ProxyListResult {
            proxies,
            total,
            page,
            per_page,
        }
    }

    pub fn stats(&self) -> ProxyPoolStats {
        let mut stats = ProxyPoolStats::default();
        for entry in self.proxies.iter() {
            stats.total += 1;
            let proxy = entry.value();
            match proxy.status {
                ProxyStatus::Valid => stats.valid += 1,
                ProxyStatus::Untested => stats.untested += 1,
                ProxyStatus::Invalid => stats.invalid += 1,
            }
            if let Some(q) = &proxy.quality {
                stats.quality_checked += 1;
                if q.chatgpt_accessible {
                    stats.chatgpt_accessible += 1;
                }
                if q.google_accessible {
                    stats.google_accessible += 1;
                }
                if q.is_residential {
                    stats.residential += 1;
                }
            }
        }
        stats
    }
}

fn proxy_matches_filter(proxy: &PoolProxy, filter: &ProxyFilter) -> bool {
    if proxy.status != ProxyStatus::Valid || proxy.local_port.is_none() {
        return false;
    }
    if let Some(ref proxy_type) = filter.proxy_type {
        if proxy.proxy_type != *proxy_type {
            return false;
        }
    }
    if filter.chatgpt
        && !proxy
            .quality
            .as_ref()
            .map(|q| q.chatgpt_accessible)
            .unwrap_or(false)
    {
        return false;
    }
    if filter.google
        && !proxy
            .quality
            .as_ref()
            .map(|q| q.google_accessible)
            .unwrap_or(false)
    {
        return false;
    }
    if filter.residential
        && !proxy
            .quality
            .as_ref()
            .map(|q| q.is_residential)
            .unwrap_or(false)
    {
        return false;
    }
    if let Some(max) = filter.risk_max {
        if !proxy
            .quality
            .as_ref()
            .map(|q| q.risk_score <= max)
            .unwrap_or(false)
        {
            return false;
        }
    }
    if let Some(ref country) = filter.country {
        if !proxy
            .quality
            .as_ref()
            .and_then(|q| q.country.as_ref())
            .map(|c| c.eq_ignore_ascii_case(country))
            .unwrap_or(false)
        {
            return false;
        }
    }
    true
}

fn proxy_matches_list_query(proxy: &PoolProxy, query: &ProxyListQuery) -> bool {
    if let Some(ref search) = query.search {
        let search = search.trim().to_ascii_lowercase();
        if !search.is_empty() {
            let ip = proxy
                .quality
                .as_ref()
                .and_then(|q| q.ip_address.as_ref())
                .map(|s| s.as_str())
                .unwrap_or("");
            if !proxy.name.to_ascii_lowercase().contains(&search)
                && !proxy.server.to_ascii_lowercase().contains(&search)
                && !ip.to_ascii_lowercase().contains(&search)
            {
                return false;
            }
        }
    }

    if let Some(ref status) = query.status {
        let status_matches = matches!(
            (status.as_str(), proxy.status),
            ("valid", ProxyStatus::Valid)
                | ("untested", ProxyStatus::Untested)
                | ("invalid", ProxyStatus::Invalid)
        );
        if !status_matches {
            return false;
        }
    }

    if let Some(ref proxy_type) = query.proxy_type {
        if !proxy_type.is_empty() && proxy.proxy_type != *proxy_type {
            return false;
        }
    }

    match query.quality.as_deref() {
        Some("chatgpt") => proxy
            .quality
            .as_ref()
            .map(|q| q.chatgpt_accessible)
            .unwrap_or(false),
        Some("google") => proxy
            .quality
            .as_ref()
            .map(|q| q.google_accessible)
            .unwrap_or(false),
        Some("residential") => proxy
            .quality
            .as_ref()
            .map(|q| q.is_residential)
            .unwrap_or(false),
        Some("unchecked") => proxy.quality.is_none(),
        _ => true,
    }
}

fn sort_proxies(proxies: &mut [PoolProxy], sort: Option<&str>, dir: Option<&str>) {
    let desc = dir == Some("desc");
    proxies.sort_by(|a, b| {
        let ord = match sort.unwrap_or("name") {
            "type" => a.proxy_type.cmp(&b.proxy_type),
            "server" => a.server.cmp(&b.server).then(a.port.cmp(&b.port)),
            "is_valid" | "status" => a.status.sort_weight().cmp(&b.status.sort_weight()),
            "error_count" => a.error_count.cmp(&b.error_count),
            "country" => quality_string(a, |q| q.country.as_deref())
                .cmp(&quality_string(b, |q| q.country.as_deref())),
            "risk" => quality_risk(a).total_cmp(&quality_risk(b)),
            _ => a.name.cmp(&b.name),
        };
        if desc {
            ord.reverse()
        } else {
            ord
        }
    });
}

fn quality_string<F>(proxy: &PoolProxy, f: F) -> String
where
    F: FnOnce(&ProxyQualityInfo) -> Option<&str>,
{
    proxy
        .quality
        .as_ref()
        .and_then(f)
        .unwrap_or("zzz")
        .to_string()
}

fn quality_risk(proxy: &PoolProxy) -> f64 {
    proxy.quality.as_ref().map(|q| q.risk_score).unwrap_or(2.0)
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct ProxyFilter {
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

#[derive(Debug, Default, serde::Deserialize)]
pub struct ProxyListQuery {
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

pub fn deserialize_opt_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => raw.parse::<usize>().map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

pub fn deserialize_opt_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => raw.parse::<f64>().map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

pub struct ProxyListResult {
    pub proxies: Vec<PoolProxy>,
    pub total: usize,
    pub page: usize,
    pub per_page: usize,
}

#[derive(Default)]
pub struct ProxyPoolStats {
    pub total: usize,
    pub valid: usize,
    pub untested: usize,
    pub invalid: usize,
    pub quality_checked: usize,
    pub chatgpt_accessible: usize,
    pub google_accessible: usize,
    pub residential: usize,
}
