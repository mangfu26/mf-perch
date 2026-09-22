//! 版本检查的端到端测试（D23），被测模块是 `src/update.rs`。
//!
//! **文件名刻意不含 `update`**：Windows 的 Installer Detection 会按文件名里
//! `update` / `install` / `setup` / `patch` 这类关键字判定"需要管理员的安装程序"，
//! 而 Rust 测试 exe 不带执行级别 manifest，于是 cargo 拉起本 target 时报
//! `请求的操作需要提升。(os error 740)`、用例 never executed。
//! 成因与差分证据见 [`docs/development-troubleshooting.md`](../../docs/development-troubleshooting.md)。
//!
//! 用本地 HTTP 服务返回真实的版本清单 JSON，验证：
//! - 能正确解析清单并识别新版本
//! - 应用版本更高时不提示降级
//! - 忽略某版本后不再提示
//! - 更新源不可达时给出可读原因
//! - 缓存生效（24 小时内不重复请求）
//!
//! 不依赖外网，因此在 CI 中也可运行。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use mf_perch_lib::update;
use tokio::io::AsyncWriteExt;

/// 启动一个只服务固定内容的最小 HTTP 服务，返回（地址，请求计数）。
async fn serve_manifest(body: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定本地端口");
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_for_task = hits.clone();

    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let hits = hits_for_task.clone();
            tokio::spawn(async move {
                // 读取请求（只需读到结束即可，不必完整解析）。
                let mut buf = [0u8; 2048];
                let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
                hits.fetch_add(1, Ordering::SeqCst);

                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    (format!("http://{addr}/manifest.json"), hits)
}

fn mem_conn() -> rusqlite::Connection {
    mf_perch_lib::store::db::open_in_memory().unwrap()
}

/// 清单正文：远端版本高于当前版本，带本平台安装包，且**显式给出发布页地址**。
fn newer_manifest() -> &'static str {
    r#"{
      "version": "99.0.0",
      "release_url": "https://github.com/mangfu26/mf-perch/releases/tag/v99.0.0",
      "notes": "测试用新版本",
      "pub_date": "2026-09-10T00:00:00Z",
      "platforms": {
        "windows-x86_64": {
          "url": "https://github.com/mangfu26/mf-perch/releases/download/v99.0.0/mf-perch.msi",
          "sha256": "deadbeef",
          "size": 2048
        }
      }
    }"#
}

/// 清单正文：**没有** `release_url` 的老形态（v0.1.2 及更早的流水线产物就是这样）。
fn legacy_manifest_without_release_url() -> &'static str {
    r#"{
      "version": "99.0.0",
      "platforms": {
        "windows-x86_64": {
          "url": "https://github.com/mangfu26/mf-perch/releases/download/v99.0.0/mf-perch_99.0.0_x64_en-US.msi",
          "sha256": "deadbeef"
        }
      }
    }"#
}

/// 清单正文：远端版本低于当前版本。
fn older_manifest() -> &'static str {
    r#"{"version": "0.0.1", "platforms": {}}"#
}

#[tokio::test]
async fn detects_new_version_and_exposes_release_page() {
    let (url, _hits) = serve_manifest(newer_manifest()).await;
    let conn = mem_conn();
    update::set_source_url(&conn, &url).unwrap();

    let status = update::check(&conn, true).await.expect("检查应成功");

    match status {
        update::UpdateStatus::Available {
            latest,
            release_url,
            sha256,
            size,
            notes,
            ..
        } => {
            assert_eq!(latest, "99.0.0");
            // 用户点开的必须是**该版本的 Release 页**（D58），不是 msi 直链：
            // 同一个 Release 下并列着 msi 与 setup.exe，直链等于替用户挑了格式。
            assert_eq!(
                release_url.as_deref(),
                Some("https://github.com/mangfu26/mf-perch/releases/tag/v99.0.0")
            );
            assert_eq!(sha256.as_deref(), Some("deadbeef"));
            assert_eq!(size, Some(2048));
            assert_eq!(notes.as_deref(), Some("测试用新版本"));
        }
        other => panic!("期望 Available，实际 {other:?}"),
    }
}

/// 线上真实存在的清单形态：v0.1.2 及更早的流水线产物里**没有** `release_url`。
/// 此时从安装包直链推导同一个 tag 的发布页，用户照样有入口可点。
#[tokio::test]
async fn legacy_manifest_without_release_url_still_yields_a_page() {
    let (url, _hits) = serve_manifest(legacy_manifest_without_release_url()).await;
    let conn = mem_conn();
    update::set_source_url(&conn, &url).unwrap();

    match update::check(&conn, true).await.unwrap() {
        update::UpdateStatus::Available { release_url, .. } => assert_eq!(
            release_url.as_deref(),
            Some("https://github.com/mangfu26/mf-perch/releases/tag/v99.0.0"),
            "老清单应推导出发布页地址"
        ),
        other => panic!("期望 Available，实际 {other:?}"),
    }
}

#[tokio::test]
async fn newer_local_version_is_reported_as_up_to_date() {
    let (url, _hits) = serve_manifest(older_manifest()).await;
    let conn = mem_conn();
    update::set_source_url(&conn, &url).unwrap();

    let status = update::check(&conn, true).await.unwrap();
    assert!(
        matches!(status, update::UpdateStatus::UpToDate { .. }),
        "远端版本更低时不应提示更新：{status:?}"
    );
}

#[tokio::test]
async fn ignored_version_is_not_reported_again() {
    let (url, _hits) = serve_manifest(newer_manifest()).await;
    let conn = mem_conn();
    update::set_source_url(&conn, &url).unwrap();
    update::ignore_version(&conn, "99.0.0").unwrap();

    let status = update::check(&conn, true).await.unwrap();
    assert!(
        matches!(status, update::UpdateStatus::Ignored { .. }),
        "被忽略的版本不应再提示：{status:?}"
    );
}

#[tokio::test]
async fn unreachable_source_yields_readable_error() {
    let conn = mem_conn();
    // 指向一个几乎必定无服务的端口。
    update::set_source_url(&conn, "http://127.0.0.1:1/manifest.json").unwrap();

    let err = update::check(&conn, true).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("更新源") || msg.contains("请求"),
        "错误信息应能让人理解发生了什么：{msg}"
    );
}

#[tokio::test]
async fn cache_prevents_repeat_requests_within_ttl() {
    let (url, hits) = serve_manifest(newer_manifest()).await;
    let conn = mem_conn();
    update::set_source_url(&conn, &url).unwrap();

    // 第一次强制检查：应当发一次请求。
    update::check(&conn, true).await.unwrap();
    let after_first = hits.load(Ordering::SeqCst);
    assert_eq!(after_first, 1, "首次检查应发起请求");

    // 自动检查（非 force）：应命中缓存，不再发请求。
    let cached = update::check(&conn, false).await.unwrap();
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "缓存有效时不应重复请求（D23：24 小时一次）"
    );
    assert!(matches!(cached, update::UpdateStatus::Available { .. }));

    // 强制检查应无视缓存，再次请求。
    update::check(&conn, true).await.unwrap();
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "手动检查应无视缓存"
    );
}

#[tokio::test]
async fn malformed_manifest_reports_failure() {
    let (url, _hits) = serve_manifest("not json at all").await;
    let conn = mem_conn();
    update::set_source_url(&conn, &url).unwrap();

    // 解析失败应作为错误上报，而不是误报"有新版本"。
    assert!(update::check(&conn, true).await.is_err());
}

#[tokio::test]
async fn manifest_without_platform_asset_still_reports_update() {
    let (url, _hits) = serve_manifest(r#"{"version": "99.0.0", "platforms": {}}"#).await;
    let conn = mem_conn();
    update::set_source_url(&conn, &url).unwrap();

    let status = update::check(&conn, true).await.unwrap();
    match status {
        update::UpdateStatus::Available {
            release_url,
            latest,
            ..
        } => {
            assert_eq!(latest, "99.0.0");
            // 没有对应平台包、清单也没给发布页地址时，仍应告知有新版本，
            // 只是界面退回到"自行到仓库 Releases 列表查找"。
            assert!(release_url.is_none());
        }
        other => panic!("期望 Available，实际 {other:?}"),
    }
}
