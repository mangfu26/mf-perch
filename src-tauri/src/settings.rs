//! 运行期可配置项（Q11 / Q12 / Q4）。
//!
//! 这些值此前散落为硬编码常量，而客户在对应问题中明确要求**可配置**：
//! - 终端配额（Q11：每主机 5 / 全局 20，可配置）
//! - 历史保留期（Q12：默认 30 天，可配置，支持永久）
//! - 命令输出与队列上限（Q4：可配置）
//!
//! 统一在此读取并落库，避免各处各读一份导致不一致。
//!
//! 数值范围做**夹紧**处理：用户可能写入 0 或极大值，
//! 直接使用会导致"配额为 0 无法建终端"或"内存被输出撑爆"这类难排查的问题。

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::domain::{
    DEFAULT_COMMAND_QUEUE_LIMIT, DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_MAX_OUTPUT_LINES,
    DEFAULT_TERMINAL_LIMIT_GLOBAL, DEFAULT_TERMINAL_LIMIT_PER_HOST,
};
use crate::error::Result;
use crate::store::db;

/// 每主机终端配额（Q11）。
pub const SETTING_QUOTA_PER_HOST: &str = "quota_per_host";
/// 全局终端配额（Q11）。
pub const SETTING_QUOTA_GLOBAL: &str = "quota_global";
/// 历史保留期小时数（Q12）；`0` 表示永久保留。
pub const SETTING_RETENTION_HOURS: &str = "history_retention_hours";
/// 单条命令输出上限（字节）。
pub const SETTING_MAX_OUTPUT_BYTES: &str = "command_max_output_bytes";
/// 单条命令输出上限（行数）。
pub const SETTING_MAX_OUTPUT_LINES: &str = "command_max_output_lines";
/// 单终端命令队列上限。
pub const SETTING_QUEUE_LIMIT: &str = "command_queue_limit";

/// 默认历史保留期：30 天（Q12 客户指定）。
pub const DEFAULT_RETENTION_HOURS: i64 = 24 * 30;
/// 保留期下限：1 小时（Q12 客户明确不支持分钟级）。
pub const MIN_RETENTION_HOURS: i64 = 1;
/// 保留期上限：10 年。超过此值等同永久，用一个有限数避免整数溢出。
pub const MAX_RETENTION_HOURS: i64 = 24 * 365 * 10;

/// 输出上限的合理边界，防止极端配置把内存撑爆。
const MIN_OUTPUT_BYTES: usize = 4 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MIN_OUTPUT_LINES: usize = 50;
const MAX_OUTPUT_LINES: usize = 500_000;
const MIN_QUEUE_LIMIT: usize = 1;
const MAX_QUEUE_LIMIT: usize = 1_000;

/// 一组运行期设置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSettings {
    pub quota_per_host: u32,
    pub quota_global: u32,
    /// 历史保留小时数；`0` 表示永久保留。
    pub retention_hours: i64,
    pub max_output_bytes: usize,
    pub max_output_lines: usize,
    pub queue_limit: usize,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            quota_per_host: DEFAULT_TERMINAL_LIMIT_PER_HOST,
            quota_global: DEFAULT_TERMINAL_LIMIT_GLOBAL,
            retention_hours: DEFAULT_RETENTION_HOURS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_output_lines: DEFAULT_MAX_OUTPUT_LINES,
            queue_limit: DEFAULT_COMMAND_QUEUE_LIMIT,
        }
    }
}

/// 把整数夹紧到闭区间。
fn clamp_i64(v: i64, lo: i64, hi: i64) -> i64 {
    v.clamp(lo, hi)
}

/// 读取一个整型设置，缺失或非法时回退默认值。
fn read_i64(conn: &Connection, key: &str, default: i64) -> i64 {
    db::get_setting(conn, key)
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

/// 读取全部运行期设置，并对数值做合法性夹紧。
pub fn load(conn: &Connection) -> Result<RuntimeSettings> {
    let d = RuntimeSettings::default();

    // 配额：至少 1，否则无法创建任何终端（这类"配置导致功能不可用"很难排查）。
    let per_host = clamp_i64(
        read_i64(conn, SETTING_QUOTA_PER_HOST, d.quota_per_host as i64),
        1,
        1_000,
    ) as u32;
    // 全局配额不应小于每主机配额，否则单主机都建不满。
    let global = clamp_i64(
        read_i64(conn, SETTING_QUOTA_GLOBAL, d.quota_global as i64),
        per_host as i64,
        100_000,
    ) as u32;

    // 保留期：Q12 明确最小 1 小时；0 保留为"永久"的语义值。
    let raw_retention = read_i64(conn, SETTING_RETENTION_HOURS, d.retention_hours);
    let retention = if raw_retention == 0 {
        0 // 永久
    } else {
        clamp_i64(raw_retention, MIN_RETENTION_HOURS, MAX_RETENTION_HOURS)
    };

    let bytes = clamp_i64(
        read_i64(conn, SETTING_MAX_OUTPUT_BYTES, d.max_output_bytes as i64),
        MIN_OUTPUT_BYTES as i64,
        MAX_OUTPUT_BYTES as i64,
    ) as usize;

    let lines = clamp_i64(
        read_i64(conn, SETTING_MAX_OUTPUT_LINES, d.max_output_lines as i64),
        MIN_OUTPUT_LINES as i64,
        MAX_OUTPUT_LINES as i64,
    ) as usize;

    let queue = clamp_i64(
        read_i64(conn, SETTING_QUEUE_LIMIT, d.queue_limit as i64),
        MIN_QUEUE_LIMIT as i64,
        MAX_QUEUE_LIMIT as i64,
    ) as usize;

    Ok(RuntimeSettings {
        quota_per_host: per_host,
        quota_global: global,
        retention_hours: retention,
        max_output_bytes: bytes,
        max_output_lines: lines,
        queue_limit: queue,
    })
}

/// 写入一项设置，返回夹紧后的实际生效值。
///
/// 返回实际值让界面能显示"你输入 0，实际生效 1"，
/// 而不是静默改写用户输入。
pub fn set(conn: &Connection, key: &str, value: &str) -> Result<String> {
    db::set_setting(conn, key, value)?;
    let effective = load(conn)?;
    Ok(match key {
        SETTING_QUOTA_PER_HOST => effective.quota_per_host.to_string(),
        SETTING_QUOTA_GLOBAL => effective.quota_global.to_string(),
        SETTING_RETENTION_HOURS => effective.retention_hours.to_string(),
        SETTING_MAX_OUTPUT_BYTES => effective.max_output_bytes.to_string(),
        SETTING_MAX_OUTPUT_LINES => effective.max_output_lines.to_string(),
        SETTING_QUEUE_LIMIT => effective.queue_limit.to_string(),
        // 其他键原样返回。
        _ => value.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_conn() -> Connection {
        db::open_in_memory().unwrap()
    }

    #[test]
    fn defaults_match_customer_decisions() {
        let conn = mem_conn();
        let s = load(&conn).unwrap();

        // Q11：每主机 5、全局 20
        assert_eq!(s.quota_per_host, 5);
        assert_eq!(s.quota_global, 20);
        // Q12：默认保留 30 天
        assert_eq!(s.retention_hours, 24 * 30);
        // Q4：队列上限 10、输出 1MB / 20000 行
        assert_eq!(s.queue_limit, 10);
        assert_eq!(s.max_output_bytes, 1024 * 1024);
        assert_eq!(s.max_output_lines, 20_000);
    }

    #[test]
    fn quota_is_configurable() {
        let conn = mem_conn();
        set(&conn, SETTING_QUOTA_PER_HOST, "8").unwrap();
        set(&conn, SETTING_QUOTA_GLOBAL, "50").unwrap();

        let s = load(&conn).unwrap();
        assert_eq!(s.quota_per_host, 8);
        assert_eq!(s.quota_global, 50);
    }

    #[test]
    fn global_quota_never_below_per_host() {
        // 全局小于每主机会让"单台主机都建不满"，属自相矛盾的配置，需自动抬升。
        let conn = mem_conn();
        set(&conn, SETTING_QUOTA_PER_HOST, "10").unwrap();
        set(&conn, SETTING_QUOTA_GLOBAL, "3").unwrap();

        let s = load(&conn).unwrap();
        assert!(
            s.quota_global >= s.quota_per_host,
            "全局配额应被抬升到不低于每主机配额，实际 {} vs {}",
            s.quota_global,
            s.quota_per_host
        );
    }

    #[test]
    fn per_host_quota_is_at_least_one() {
        // 配额为 0 会导致完全无法创建终端，且原因难以察觉。
        let conn = mem_conn();
        let effective = set(&conn, SETTING_QUOTA_PER_HOST, "0").unwrap();
        assert_eq!(effective, "1", "应夹紧到 1 并把实际值告知调用方");
        assert_eq!(load(&conn).unwrap().quota_per_host, 1);
    }

    #[test]
    fn retention_honours_minimum_of_one_hour() {
        // Q12：不支持分钟级，最小 1 小时。
        let conn = mem_conn();
        set(&conn, SETTING_RETENTION_HOURS, "0").unwrap();
        // 0 是"永久"的语义值，不应被夹紧成 1。
        assert_eq!(load(&conn).unwrap().retention_hours, 0);

        // 但 1 以下的正数（不可能由界面产生，防手改配置）应夹到 1。
        set(&conn, SETTING_RETENTION_HOURS, "-5").unwrap();
        assert_eq!(load(&conn).unwrap().retention_hours, 1);
    }

    #[test]
    fn retention_allows_large_values_meaning_permanent() {
        let conn = mem_conn();
        set(&conn, SETTING_RETENTION_HOURS, "100000").unwrap();
        let s = load(&conn).unwrap();
        assert_eq!(s.retention_hours, MAX_RETENTION_HOURS, "应夹到上限而非溢出");
    }

    #[test]
    fn output_limits_are_clamped() {
        let conn = mem_conn();
        // 极小值会让输出几乎不可用，极大值会撑爆内存，两头都要夹。
        set(&conn, SETTING_MAX_OUTPUT_BYTES, "1").unwrap();
        assert_eq!(load(&conn).unwrap().max_output_bytes, MIN_OUTPUT_BYTES);

        set(&conn, SETTING_MAX_OUTPUT_BYTES, "999999999").unwrap();
        assert_eq!(load(&conn).unwrap().max_output_bytes, MAX_OUTPUT_BYTES);

        set(&conn, SETTING_MAX_OUTPUT_LINES, "1").unwrap();
        assert_eq!(load(&conn).unwrap().max_output_lines, MIN_OUTPUT_LINES);
    }

    #[test]
    fn queue_limit_is_clamped() {
        let conn = mem_conn();
        set(&conn, SETTING_QUEUE_LIMIT, "0").unwrap();
        assert_eq!(load(&conn).unwrap().queue_limit, MIN_QUEUE_LIMIT);

        set(&conn, SETTING_QUEUE_LIMIT, "999999").unwrap();
        assert_eq!(load(&conn).unwrap().queue_limit, MAX_QUEUE_LIMIT);
    }

    #[test]
    fn invalid_values_fall_back_to_defaults() {
        let conn = mem_conn();
        // 手改数据库写入非数字时，不应导致读取失败或功能异常。
        db::set_setting(&conn, SETTING_QUOTA_PER_HOST, "abc").unwrap();
        db::set_setting(&conn, SETTING_RETENTION_HOURS, "??").unwrap();

        let s = load(&conn).unwrap();
        assert_eq!(s.quota_per_host, DEFAULT_TERMINAL_LIMIT_PER_HOST);
        assert_eq!(s.retention_hours, DEFAULT_RETENTION_HOURS);
    }
}
