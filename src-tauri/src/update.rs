//! 更新检查（D23 / Q21 / Q34）。
//!
//! 设计约束来自客户决定：
//! - **不做自动下载与自动安装**——只检查版本、提示用户去下载页
//! - 版本源为**可配置 URL**（默认指向客户的 Gist），便于换源或指向镜像
//! - 检查**异步且静默失败**：网络不通时不打扰用户，只记日志
//! - 结果缓存 24 小时；手动检查不受缓存限制
//! - 语义化版本比对；**应用版本高于远端时不提示**（避免开发版被"降级"提醒）
//! - 支持"忽略此版本"，同一版本不再重复提示
//! - 可关闭自动检查
//! - 不自动打开浏览器，仅返回下载地址由用户点击
//! - 返回 SHA256 供用户核对下载完整性

use std::collections::HashMap;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::store::db;

/// `settings` 键：更新源地址（可配置）。
pub const SETTING_SOURCE_URL: &str = "update_source_url";
/// `settings` 键：上次检查时间（RFC3339）。
pub const SETTING_LAST_CHECK: &str = "update_last_check_at";
/// `settings` 键：上次检查结果缓存（JSON）。
pub const SETTING_CACHED: &str = "update_cached_result";
/// `settings` 键：用户选择忽略的版本。
pub const SETTING_IGNORED_VERSION: &str = "update_ignored_version";
/// `settings` 键：是否启动时自动检查。
pub const SETTING_AUTO_CHECK: &str = "update_auto_check";
/// `settings` 键：当前应用版本（记录用于比对，发布打包时写入）。
pub const SETTING_CURRENT_VERSION: &str = "update_current_version";

/// 缓存有效期（D23：默认每 24 小时最多自动检查一次）。
pub const CACHE_TTL_HOURS: i64 = 24;
/// 单次请求超时（D23：5 秒）。
pub const REQUEST_TIMEOUT_SECS: u64 = 5;

/// 本平台标识，用于在清单中选择对应安装包。
///
/// 与 Tauri 的目标三元组风格一致，便于将来扩展多平台。
pub fn platform_key() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "windows-x86_64"
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        "windows-aarch64"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "darwin-aarch64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "darwin-x86_64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x86_64"
    } else {
        "unknown"
    }
}

/// 远端版本清单（客户在 Gist 中维护的 JSON）。
///
/// 字段全部为可选或带默认值，使清单可以逐步完善，
/// 缺字段不会导致整个检查失败。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpdateManifest {
    /// 最新版本号（语义化版本，如 `0.2.0`；也允许带 `v` 前缀）。
    pub version: String,
    /// 发布说明（支持多行文本）。
    #[serde(default)]
    pub notes: Option<String>,
    /// 发布时间（RFC3339）。
    #[serde(default)]
    pub pub_date: Option<String>,
    /// 各平台的安装包信息。
    #[serde(default)]
    pub platforms: HashMap<String, PlatformAsset>,
}

/// 某平台的安装包信息。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlatformAsset {
    /// 下载地址。
    pub url: String,
    /// 安装包 SHA256，供用户核对完整性。
    #[serde(default)]
    pub sha256: Option<String>,
    /// 文件大小（字节）。
    #[serde(default)]
    pub size: Option<u64>,
}

/// 检查结果（返回给界面）。
///
/// 用 `tag` 区分状态，前端据此展示不同提示。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UpdateStatus {
    /// 已是最新版本。
    UpToDate {
        current: String,
        latest: String,
    },
    /// 有新版本可用。
    Available {
        current: String,
        latest: String,
        notes: Option<String>,
        published_at: Option<String>,
        download_url: Option<String>,
        sha256: Option<String>,
        size: Option<u64>,
    },
    /// 该版本已被用户忽略，不再提示。
    Ignored {
        current: String,
        latest: String,
    },
    /// 检查失败（网络或格式问题）。
    Failed {
        reason: String,
    },
}

/// 默认更新源地址。
///
/// 客户尚未提供 Gist 地址，因此默认置空：此时检查会明确返回
/// "未配置更新源"，而不是静默无反应（P1：明确报错）。
pub const DEFAULT_SOURCE_URL: &str = "";

/// 读取更新源地址（可配置）。
pub fn source_url(conn: &Connection) -> Result<String> {
    Ok(db::get_setting(conn, SETTING_SOURCE_URL)?
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SOURCE_URL.to_string()))
}

pub fn set_source_url(conn: &Connection, url: &str) -> Result<()> {
    db::set_setting(conn, SETTING_SOURCE_URL, url.trim())
}

/// 是否启用启动时自动检查。
pub fn auto_check_enabled(conn: &Connection) -> Result<bool> {
    Ok(db::get_setting(conn, SETTING_AUTO_CHECK)?
        .map(|v| v != "false")
        .unwrap_or(true))
}

pub fn set_auto_check(conn: &Connection, enabled: bool) -> Result<()> {
    db::set_setting(conn, SETTING_AUTO_CHECK, if enabled { "true" } else { "false" })
}

/// 读取用户忽略的版本。
pub fn ignored_version(conn: &Connection) -> Result<Option<String>> {
    Ok(db::get_setting(conn, SETTING_IGNORED_VERSION)?.filter(|s| !s.trim().is_empty()))
}

/// 记录用户忽略某版本（同版本不再提示）。
pub fn ignore_version(conn: &Connection, version: &str) -> Result<()> {
    db::set_setting(conn, SETTING_IGNORED_VERSION, version.trim())
}

/// 当前应用版本。
pub fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 缓存是否仍然有效（24 小时内）。
pub fn cache_is_fresh(conn: &Connection) -> Result<bool> {
    let Some(last) = db::get_setting(conn, SETTING_LAST_CHECK)? else {
        return Ok(false);
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&last) else {
        return Ok(false);
    };
    let age = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
    Ok(age.num_hours() < CACHE_TTL_HOURS)
}

/// 读取缓存的结果。
pub fn cached_result(conn: &Connection) -> Result<Option<UpdateStatus>> {
    match db::get_setting(conn, SETTING_CACHED)? {
        Some(json) => Ok(serde_json::from_str(&json).ok()),
        None => Ok(None),
    }
}

/// 写入缓存结果并记录检查时间。
fn store_result(conn: &Connection, status: &UpdateStatus) -> Result<()> {
    let json = serde_json::to_string(status)?;
    db::set_setting(conn, SETTING_CACHED, &json)?;
    db::set_setting(conn, SETTING_LAST_CHECK, &chrono::Utc::now().to_rfc3339())?;
    Ok(())
}

/// 供上层（应用启动时的自动检查）回写缓存。
pub fn store_cached(conn: &Connection, status: &UpdateStatus) -> Result<()> {
    store_result(conn, status)
}

/// 解析版本字符串：容忍 `v` 前缀与空白。
pub fn parse_version(raw: &str) -> Option<semver::Version> {
    let trimmed = raw.trim().trim_start_matches('v').trim();
    semver::Version::parse(trimmed).ok()
}

/// 比较当前版本与远端版本，判定是否有更新。
///
/// 关键行为：
/// - 远端版本**更高**才算有更新；
/// - 相等或无更新返回 `UpToDate`；
/// - **当前版本更高时也返回 UpToDate**，避免开发版被"降级"提示；
/// - 任一侧版本号无法解析时，保守地判为无更新并记录原因，
///   而不是误报有新版本（假警报会让用户失去信任）。
pub fn compare_versions(current: &str, latest: &str) -> Result<std::cmp::Ordering> {
    let cur = parse_version(current).ok_or_else(|| {
        AppError::Config(format!("当前版本号无法解析：{current}"))
    })?;
    let lat = parse_version(latest).ok_or_else(|| {
        AppError::Config(format!("远端版本号无法解析：{latest}"))
    })?;
    Ok(lat.cmp(&cur))
}

/// 由清单与当前版本推导检查结果（纯逻辑，便于测试）。
pub fn evaluate(
    manifest: &UpdateManifest,
    current: &str,
    ignored: Option<&str>,
    platform: &str,
) -> UpdateStatus {
    // 版本无法解析时保守判为最新，并说明原因。
    let ordering = match compare_versions(current, &manifest.version) {
        Ok(o) => o,
        Err(e) => {
            return UpdateStatus::Failed {
                reason: e.to_string(),
            }
        }
    };

    if ordering != std::cmp::Ordering::Greater {
        return UpdateStatus::UpToDate {
            current: current.to_string(),
            latest: manifest.version.clone(),
        };
    }

    // 用户忽略过该版本则不再提示。
    if ignored.map(|v| v.trim()) == Some(manifest.version.trim()) {
        return UpdateStatus::Ignored {
            current: current.to_string(),
            latest: manifest.version.clone(),
        };
    }

    let asset = manifest.platforms.get(platform);

    UpdateStatus::Available {
        current: current.to_string(),
        latest: manifest.version.clone(),
        notes: manifest.notes.clone(),
        published_at: manifest.pub_date.clone(),
        download_url: asset.map(|a| a.url.clone()),
        sha256: asset.and_then(|a| a.sha256.clone()),
        size: asset.and_then(|a| a.size),
    }
}

/// 从远端拉取清单并解析。
pub async fn fetch_manifest(url: &str) -> Result<UpdateManifest> {
    if url.trim().is_empty() {
        return Err(AppError::Config(
            "尚未配置更新源地址。请在设置中填写版本清单的地址。".into(),
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        // 明确使用 rustls，避免构建时依赖系统 TLS 库。
        .build()
        .map_err(|e| AppError::Config(format!("创建 HTTP 客户端失败：{e}")))?;

    let resp = client
        .get(url)
        .header("Accept", "application/json")
        // GitHub Gist 的 raw 地址对 User-Agent 有要求。
        .header("User-Agent", concat!("mf-perch/", env!("CARGO_PKG_VERSION")))
        .send()
        .await
        .map_err(|e| AppError::Config(format!("请求更新源失败：{e}")))?;

    if !resp.status().is_success() {
        return Err(AppError::Config(format!(
            "更新源返回 HTTP {}",
            resp.status().as_u16()
        )));
    }

    resp.json::<UpdateManifest>()
        .await
        .map_err(|e| AppError::Config(format!("更新源返回的内容无法解析：{e}")))
}

/// 执行一次完整的更新检查。
///
/// `force` 为真表示用户手动触发：忽略缓存，且失败时报告原因。
/// 为假表示启动时的自动检查：命中缓存则直接返回，失败静默降级。
pub async fn check(conn: &Connection, force: bool) -> Result<UpdateStatus> {
    let current = current_version();

    // 自动检查命中缓存则直接复用（D23：24 小时一次）。
    if !force && cache_is_fresh(conn)? {
        if let Some(cached) = cached_result(conn)? {
            tracing::debug!("更新检查命中缓存");
            return Ok(cached);
        }
    }

    let url = source_url(conn)?;
    let ignored = ignored_version(conn)?;

    match fetch_manifest(&url).await {
        Ok(manifest) => {
            let status = evaluate(&manifest, &current, ignored.as_deref(), platform_key());
            store_result(conn, &status)?;
            Ok(status)
        }
        Err(e) => {
            // 自动检查失败时静默：网络不通属于常见情况，不该打扰用户。
            if !force {
                tracing::debug!("自动更新检查失败（已忽略）：{e}");
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_conn() -> Connection {
        db::open_in_memory().unwrap()
    }

    fn manifest(version: &str) -> UpdateManifest {
        UpdateManifest {
            version: version.to_string(),
            notes: Some("修复若干问题".into()),
            pub_date: Some("2026-09-10T00:00:00Z".into()),
            platforms: HashMap::new(),
        }
    }

    fn manifest_with_asset(version: &str, platform: &str) -> UpdateManifest {
        let mut m = manifest(version);
        m.platforms.insert(
            platform.to_string(),
            PlatformAsset {
                url: "https://example.com/pkg.msi".into(),
                sha256: Some("abc123".into()),
                size: Some(1024),
            },
        );
        m
    }

    #[test]
    fn parse_version_tolerates_v_prefix_and_whitespace() {
        assert!(parse_version("v0.2.0").is_some());
        assert!(parse_version(" 0.2.0 ").is_some());
        assert!(parse_version("0.2.0").is_some());
        assert!(parse_version("not-a-version").is_none());
        assert!(parse_version("").is_none());
    }

    #[test]
    fn detects_newer_remote_version() {
        assert_eq!(
            compare_versions("0.1.0", "0.2.0").unwrap(),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("0.1.0", "0.1.1").unwrap(),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_versions("1.0.0", "2.0.0").unwrap(),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn same_version_is_not_an_update() {
        assert_eq!(
            compare_versions("0.1.0", "0.1.0").unwrap(),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn newer_local_version_is_not_downgraded() {
        // 开发版比线上版本高时，绝不能被提示"有更新"（否则会被诱导降级）。
        let s = evaluate(&manifest("0.1.0"), "0.2.0", None, "windows-x86_64");
        assert!(
            matches!(s, UpdateStatus::UpToDate { .. }),
            "本地版本更高时应判为最新：{s:?}"
        );
    }

    #[test]
    fn reports_available_with_asset_details() {
        let m = manifest_with_asset("0.9.0", "windows-x86_64");
        let s = evaluate(&m, "0.1.0", None, "windows-x86_64");

        match s {
            UpdateStatus::Available {
                latest,
                download_url,
                sha256,
                size,
                ..
            } => {
                assert_eq!(latest, "0.9.0");
                assert_eq!(download_url.as_deref(), Some("https://example.com/pkg.msi"));
                assert_eq!(sha256.as_deref(), Some("abc123"));
                assert_eq!(size, Some(1024));
            }
            other => panic!("期望 Available，实际 {other:?}"),
        }
    }

    #[test]
    fn available_without_matching_platform_still_reports() {
        // 清单里没有当前平台的包时，仍应告知有新版本，只是没有下载地址。
        let m = manifest_with_asset("0.9.0", "linux-x86_64");
        let s = evaluate(&m, "0.1.0", None, "windows-x86_64");
        match s {
            UpdateStatus::Available {
                download_url,
                sha256,
                ..
            } => {
                assert!(download_url.is_none());
                assert!(sha256.is_none());
            }
            other => panic!("期望 Available，实际 {other:?}"),
        }
    }

    #[test]
    fn ignored_version_suppresses_notice() {
        let m = manifest("0.9.0");
        let s = evaluate(&m, "0.1.0", Some("0.9.0"), "windows-x86_64");
        assert!(
            matches!(s, UpdateStatus::Ignored { .. }),
            "被忽略的版本不应再提示：{s:?}"
        );
    }

    #[test]
    fn ignore_only_applies_to_that_version() {
        let m = manifest("0.9.1");
        let s = evaluate(&m, "0.1.0", Some("0.9.0"), "windows-x86_64");
        assert!(
            matches!(s, UpdateStatus::Available { .. }),
            "新版本仍应提示：{s:?}"
        );
    }

    #[test]
    fn unparsable_remote_version_fails_conservatively() {
        // 宁可报错也不误报有更新：假警报会让用户不再相信更新提示。
        let s = evaluate(&manifest("beta-x"), "0.1.0", None, "windows-x86_64");
        assert!(matches!(s, UpdateStatus::Failed { .. }), "实际：{s:?}");
    }

    #[test]
    fn source_url_defaults_to_empty_when_unset() {
        let conn = mem_conn();
        assert_eq!(source_url(&conn).unwrap(), DEFAULT_SOURCE_URL);
    }

    #[test]
    fn source_url_is_configurable() {
        let conn = mem_conn();
        set_source_url(&conn, "https://gist.example.com/raw/abc").unwrap();
        assert_eq!(source_url(&conn).unwrap(), "https://gist.example.com/raw/abc");

        // 空白值应回退到默认，避免把空地址当成有效配置。
        set_source_url(&conn, "   ").unwrap();
        assert_eq!(source_url(&conn).unwrap(), DEFAULT_SOURCE_URL);
    }

    #[tokio::test]
    async fn fetch_rejects_empty_url_with_clear_reason() {
        let err = fetch_manifest("").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("更新源"), "应提示未配置更新源：{msg}");
    }

    #[test]
    fn auto_check_defaults_to_enabled() {
        let conn = mem_conn();
        assert!(auto_check_enabled(&conn).unwrap());
        set_auto_check(&conn, false).unwrap();
        assert!(!auto_check_enabled(&conn).unwrap());
    }

    #[test]
    fn cache_is_stale_when_never_checked() {
        let conn = mem_conn();
        assert!(!cache_is_fresh(&conn).unwrap());
    }

    #[test]
    fn cache_is_fresh_right_after_check() {
        let conn = mem_conn();
        let status = UpdateStatus::UpToDate {
            current: current_version(),
            latest: current_version(),
        };
        store_result(&conn, &status).unwrap();
        assert!(cache_is_fresh(&conn).unwrap());
    }

    #[test]
    fn stale_cache_is_detected() {
        let conn = mem_conn();
        // 手工写入一个 30 小时前的时间戳。
        let old = (chrono::Utc::now() - chrono::Duration::hours(30)).to_rfc3339();
        db::set_setting(&conn, SETTING_LAST_CHECK, &old).unwrap();
        assert!(!cache_is_fresh(&conn).unwrap());
    }

    #[test]
    fn cached_result_roundtrips() {
        let conn = mem_conn();
        let status = UpdateStatus::Available {
            current: "0.1.0".into(),
            latest: "0.2.0".into(),
            notes: Some("note".into()),
            published_at: None,
            download_url: Some("https://x/y".into()),
            sha256: None,
            size: None,
        };
        store_result(&conn, &status).unwrap();

        let loaded = cached_result(&conn).unwrap().expect("应有缓存");
        match loaded {
            UpdateStatus::Available { latest, .. } => assert_eq!(latest, "0.2.0"),
            other => panic!("缓存内容不符：{other:?}"),
        }
    }

    #[test]
    fn ignore_version_persists_and_clears_on_empty() {
        let conn = mem_conn();
        ignore_version(&conn, "0.9.0").unwrap();
        assert_eq!(ignored_version(&conn).unwrap().as_deref(), Some("0.9.0"));

        // 写入空白应视为清除。
        ignore_version(&conn, "  ").unwrap();
        assert!(ignored_version(&conn).unwrap().is_none());
    }

    #[test]
    fn platform_key_is_not_unknown_on_supported_targets() {
        // 若为 unknown，说明遗漏了当前平台，检查将永远找不到安装包。
        #[cfg(target_os = "windows")]
        assert_ne!(platform_key(), "unknown");
    }

    #[test]
    fn current_version_matches_package_version() {
        assert_eq!(current_version(), env!("CARGO_PKG_VERSION"));
    }
}
