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
    platform_key_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// 平台标识的映射规则本身（与编译目标无关，因此可以对**全部**平台做测试）。
///
/// 参数取自 `std::env::consts::OS` / `ARCH`，取值如 `"windows"` / `"x86_64"`。
/// 未列出的组合返回 `"unknown"`：更新检查会因此找不到安装包——
/// 这正是"漏配平台"应有的显式表现，而不是静默取到别的平台的包。
fn platform_key_for(os: &str, arch: &str) -> &'static str {
    match (os, arch) {
        ("windows", "x86_64") => "windows-x86_64",
        ("windows", "aarch64") => "windows-aarch64",
        ("macos", "aarch64") => "darwin-aarch64",
        ("macos", "x86_64") => "darwin-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        _ => "unknown",
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

/// 内置默认更新源地址（客户维护的 Gist 清单）。
///
/// 设计取舍（D42）：
/// - **默认可用**：以前默认留空，用户既不知道地址、自动检查也从不生效，
///   等于"更新功能默认不存在"。现在内置一个默认值，开箱即可检查；
/// - **仍可覆盖**：设置页可改成镜像或其它源（客户要求保留灵活性）；
/// - **用不带修订号的 raw 地址**：
///   `.../raw/<文件名>` 而不是 `.../raw/<一长串SHA>/<文件名>`。
///   前者始终返回最新修订，因此**只改 Gist 内容就能让所有用户收到更新**；
///   后者钉死在某一版，改了 Gist 老用户永远看不到（实测两种形式都返回 200、
///   不跳转，所以这里选前者）；
/// - 代价：**改这个默认值需要发新版**——地址真要变时，老版本用户需手动改设置，
///   这正是保留可编辑入口的价值。
pub const DEFAULT_SOURCE_URL: &str =
    "https://gist.githubusercontent.com/mangfu26/311c09b123911b6a4860483a645a4eb0/raw/mf-perch-update.json";

/// 读取**生效的**更新源地址。
///
/// 用户配置为空（未设置或已清空）时回退到 [`DEFAULT_SOURCE_URL`]。
pub fn source_url(conn: &Connection) -> Result<String> {
    Ok(db::get_setting(conn, SETTING_SOURCE_URL)?
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SOURCE_URL.to_string()))
}

/// 用户是否**自定义**过更新源（false 表示正在使用内置默认值）。
///
/// 界面据此显示"当前使用内置默认地址"，并决定「恢复默认」是否需要提示。
/// 刻意不把默认地址复制到前端：地址只在 Rust 侧维护一处（单一事实来源）。
pub fn source_is_custom(conn: &Connection) -> Result<bool> {
    Ok(db::get_setting(conn, SETTING_SOURCE_URL)?
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false))
}

pub fn set_source_url(conn: &Connection, url: &str) -> Result<()> {
    let url = url.trim();
    // 只接受 http(s)，挡住 file://、javascript: 等其它协议（V16）。
    // 空串表示"清除配置"，允许通过。
    if !url.is_empty() {
        validate_source_url(url)?;
    }
    db::set_setting(conn, SETTING_SOURCE_URL, url)
}

/// 校验更新源地址的协议（V16）。
fn validate_source_url(url: &str) -> Result<()> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        Ok(())
    } else {
        Err(AppError::Config(format!(
            "更新源地址必须以 http:// 或 https:// 开头：{url}"
        )))
    }
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

    // URL 可能来自历史配置（设置时尚未校验协议），这里再挡一次（V16）。
    validate_source_url(url)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        // 不跟随重定向：避免被重定向到内网地址（SSRF 探测）或其它主机，
        // 也让"清单来源"始终等于用户配置并确认过的那个地址（V16）。
        .redirect(reqwest::redirect::Policy::none())
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
        // 重定向已禁用，3xx 会走到这里并给出可读提示。
        return Err(AppError::Config(format!(
            "更新源返回 HTTP {}（若为 3xx，请填写该地址的最终跳转目标）",
            resp.status().as_u16()
        )));
    }

    // 限制响应体大小（V15）：清单是几十行的 JSON，正常远小于此上限；
    // 若不限制，恶意/被劫持的更新源可返回超大 body 撑爆内存。
    // 用流式读取逐块累计，超限立即中止，不把整个 body 读进内存。
    const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

    if let Some(len) = resp.content_length() {
        if len > MAX_MANIFEST_BYTES as u64 {
            return Err(AppError::Config(format!(
                "更新源返回的内容过大（{len} 字节），已拒绝解析"
            )));
        }
    }

    let mut body: Vec<u8> = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| AppError::Config(format!("读取更新源内容失败：{e}")))?
    {
        if body.len() + chunk.len() > MAX_MANIFEST_BYTES {
            return Err(AppError::Config(format!(
                "更新源返回的内容超过 {} KiB 上限，已中止读取",
                MAX_MANIFEST_BYTES / 1024
            )));
        }
        body.extend_from_slice(&chunk);
    }

    serde_json::from_slice::<UpdateManifest>(&body)
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
    fn source_url_rejects_non_http_schemes() {
        // V16：挡住 file://、javascript: 等协议，避免本地文件读取等手段。
        let conn = mem_conn();
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://example.com/m.json",
            "data:text/plain,hi",
        ] {
            assert!(
                set_source_url(&conn, bad).is_err(),
                "{bad} 不应被接受为更新源"
            );
        }
        // 空串表示清除配置，应允许。
        assert!(set_source_url(&conn, "").is_ok());
        // http/https 正常接受。
        assert!(set_source_url(&conn, "https://example.com/m.json").is_ok());
        assert!(set_source_url(&conn, "http://127.0.0.1:8000/m.json").is_ok());
    }

    #[test]
    fn source_url_is_trimmed_before_storage() {
        let conn = mem_conn();
        set_source_url(&conn, "  https://example.com/m.json  ").unwrap();
        assert_eq!(
            source_url(&conn).unwrap(),
            "https://example.com/m.json",
            "首尾空白应被去除"
        );
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
    fn source_url_falls_back_to_builtin_default() {
        // D42：未配置时也应有一个可用的更新源，否则更新功能默认等于不存在。
        let conn = mem_conn();
        assert_eq!(source_url(&conn).unwrap(), DEFAULT_SOURCE_URL);
        assert!(
            DEFAULT_SOURCE_URL.starts_with("https://"),
            "内置默认必须是 https：{DEFAULT_SOURCE_URL}"
        );
        assert!(!source_is_custom(&conn).unwrap(), "默认值不算自定义");
    }

    #[test]
    fn builtin_default_uses_unpinned_raw_url() {
        // 关键：raw 地址**不带修订号（SHA）**，这样只改 Gist 内容即可让所有用户
        // 收到更新；带 SHA 的形式会钉死在某一版（客户实测时给的就是带 SHA 的）。
        let raw = DEFAULT_SOURCE_URL
            .split("/raw/")
            .nth(1)
            .expect("默认地址应包含 /raw/");
        assert_eq!(
            raw.matches('/').count(),
            0,
            "raw 之后应直接是文件名，不应再有一层修订号目录：{DEFAULT_SOURCE_URL}"
        );
        assert!(raw.ends_with(".json"), "应指向清单文件：{raw}");
    }

    #[test]
    fn custom_source_is_reported_as_custom() {
        let conn = mem_conn();
        set_source_url(&conn, "https://mirror.example.com/m.json").unwrap();
        assert!(source_is_custom(&conn).unwrap());
        assert_eq!(
            source_url(&conn).unwrap(),
            "https://mirror.example.com/m.json",
            "自定义值应覆盖默认"
        );

        // 清空后回到默认（「恢复默认」正是这么实现的）。
        set_source_url(&conn, "").unwrap();
        assert!(!source_is_custom(&conn).unwrap());
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

    /// 清单里已支持的平台必须逐个映射正确：漏掉一个，该平台的更新检查就永远找不到安装包。
    ///
    /// 这里对**全部**平台断言。原先用 `#[cfg(target_os = "windows")]` 包住断言行，
    /// 在 macOS / Linux 上测试体为空、恒绿——等于没测（§5.3 的"绿灯假象"）。
    #[test]
    fn platform_key_maps_every_supported_target() {
        let cases = [
            ("windows", "x86_64", "windows-x86_64"),
            ("windows", "aarch64", "windows-aarch64"),
            ("macos", "aarch64", "darwin-aarch64"),
            ("macos", "x86_64", "darwin-x86_64"),
            ("linux", "x86_64", "linux-x86_64"),
        ];
        for (os, arch, expected) in cases {
            assert_eq!(platform_key_for(os, arch), expected, "{os}/{arch} 映射错误");
        }

        assert_eq!(
            platform_key_for("freebsd", "x86_64"),
            "unknown",
            "未知平台应显式回落为 unknown，而不是取到别的平台的包"
        );
    }

    /// 当前构建必须落在已支持的组合里（项目宣称跨平台打包，任一平台漏配都会让更新检查失效）。
    #[test]
    fn platform_key_is_known_on_this_build() {
        assert_ne!(
            platform_key(),
            "unknown",
            "当前构建平台（{}/{}）未在清单映射中",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    }

    #[test]
    fn current_version_matches_package_version() {
        assert_eq!(current_version(), env!("CARGO_PKG_VERSION"));
    }
}
