//! 输出累积与截断（Q4 / D14）。
//!
//! 命令输出可能极大（构建日志、`apt` 输出等），必须设上限，
//! 否则会撑爆 MCP 上下文与内存。截断时**明确标注** `truncated`
//! 并保留真实总字节数，让人与 Agent 都知道输出不完整。

use crate::domain::{DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_MAX_OUTPUT_LINES};

/// 输出累积器。
///
/// 同时按**字节数**与**行数**设限，任一先到即停止累积。
/// 之所以两个维度都要：单行超长（如 base64 数据）会先撞字节上限，
/// 而海量短行会先撞行数上限。
#[derive(Debug, Clone)]
pub struct OutputAccumulator {
    bytes: Vec<u8>,
    max_bytes: usize,
    max_lines: usize,
    line_count: usize,
    truncated: bool,
    /// 已接收但未计入 `bytes` 的总字节数（截断后仍继续统计，用于告知真实规模）。
    total_bytes: u64,
    /// 截断后仍继续计数总行数。
    total_lines: u64,
}

impl Default for OutputAccumulator {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_MAX_OUTPUT_LINES)
    }
}

impl OutputAccumulator {
    pub fn new(max_bytes: usize, max_lines: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
            max_lines,
            line_count: 0,
            truncated: false,
            total_bytes: 0,
            total_lines: 0,
        }
    }

    /// 追加一段数据。
    pub fn push_bytes(&mut self, data: &[u8]) {
        self.total_bytes += data.len() as u64;

        if self.truncated {
            return;
        }
        if self.bytes.len() + data.len() > self.max_bytes {
            self.truncated = true;
            return;
        }
        self.bytes.extend_from_slice(data);
    }

    /// 追加一行输出（不含换行符，由累积器补回）。
    ///
    /// 行数上限在此处生效——这样即使每行都很短，
    /// 也不会因为行数爆炸而耗尽内存与上下文。
    pub fn push_line(&mut self, line: &str) {
        self.total_lines += 1;
        // 无论是否已截断，都如实统计接收到的总字节数。
        self.total_bytes += (line.len() + 1) as u64;

        if self.truncated {
            return;
        }
        if self.line_count >= self.max_lines {
            self.truncated = true;
            return;
        }

        let needed = line.len() + 1;
        if self.bytes.len() + needed > self.max_bytes {
            self.truncated = true;
            return;
        }

        self.bytes.extend_from_slice(line.as_bytes());
        self.bytes.push(b'\n');
        self.line_count += 1;
    }

    /// 是否已发生截断。
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// 已累积输出的字节数（截断前的实际保留量）。
    pub fn retained_bytes(&self) -> usize {
        self.bytes.len()
    }

    /// 接收到的输出总字节数（即使被截断也如实统计）。
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// 接收到的输出总行数。
    pub fn total_lines(&self) -> u64 {
        self.total_lines
    }

    /// 取出当前累积内容（不强求 UTF-8，按有损方式转换以避免因半个字符丢整段输出）。
    pub fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.bytes).to_string()
    }

    /// 取出累积内容并在末尾追加截断说明，便于人类与 Agent 都看到提示。
    pub fn to_display_string(&self) -> String {
        let mut s = self.to_string_lossy();
        if self.truncated {
            s.push_str(&format!(
                "\n[mf-perch] 输出已截断：已保留 {} 字节 / {} 行，实际共约 {} 字节 / {} 行\n",
                self.retained_bytes(),
                self.line_count,
                self.total_bytes,
                self.total_lines
            ));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_lines_in_order() {
        let mut acc = OutputAccumulator::new(1024, 100);
        acc.push_line("first");
        acc.push_line("second");
        assert_eq!(acc.to_string_lossy(), "first\nsecond\n");
        assert!(!acc.is_truncated());
    }

    #[test]
    fn byte_limit_truncates_and_flags() {
        // 上限 10 字节。
        let mut acc = OutputAccumulator::new(10, 1000);
        acc.push_line("123456"); // 7 字节（含换行）
        assert!(!acc.is_truncated());
        acc.push_line("789012"); // 会超过上限
        assert!(acc.is_truncated());
        assert_eq!(acc.retained_bytes(), 7);
    }

    #[test]
    fn line_limit_truncates_even_with_tiny_lines() {
        // 行数上限 3，字节上限很大。
        let mut acc = OutputAccumulator::new(1024 * 1024, 3);
        for i in 0..5 {
            acc.push_line(&format!("l{i}"));
        }
        assert!(acc.is_truncated());
        assert_eq!(acc.to_string_lossy(), "l0\nl1\nl2\n");
        // 总行数仍如实统计 5 行。
        assert_eq!(acc.total_lines(), 5);
    }

    #[test]
    fn total_bytes_counts_everything_even_after_truncation() {
        let mut acc = OutputAccumulator::new(5, 1000);
        acc.push_line("abc"); // 4 字节，保留
        acc.push_line("defgh"); // 触发截断
        acc.push_line("ijklm"); // 截断后仍应计入总量
        assert!(acc.is_truncated());
        // 4 + 6 + 6 = 16
        assert_eq!(acc.total_bytes(), 16);
        assert_eq!(acc.retained_bytes(), 4);
    }

    #[test]
    fn display_string_mentions_truncation() {
        let mut acc = OutputAccumulator::new(4, 1000);
        acc.push_line("abcdef");
        let s = acc.to_display_string();
        assert!(s.contains("已截断"), "截断必须明确标注（D14）");
        assert!(s.contains("实际共约"));
    }

    #[test]
    fn display_string_clean_when_not_truncated() {
        let mut acc = OutputAccumulator::new(1024, 100);
        acc.push_line("ok");
        assert_eq!(acc.to_display_string(), "ok\n");
    }

    #[test]
    fn push_bytes_handles_partial_utf8_safely() {
        // 模拟分块接收：多字节字符可能被切开。
        let text = "中文输出";
        let bytes = text.as_bytes();
        let mut acc = OutputAccumulator::new(1024, 100);
        // 故意在字符中间切分。
        acc.push_bytes(&bytes[..4]);
        acc.push_bytes(&bytes[4..]);
        assert_eq!(acc.to_string_lossy(), text);
    }

    #[test]
    fn exact_byte_boundary_is_not_truncated() {
        // 恰好等于上限不应算截断。
        let mut acc = OutputAccumulator::new(4, 100);
        acc.push_line("abc"); // 恰好 4 字节
        assert!(!acc.is_truncated());
        assert_eq!(acc.to_string_lossy(), "abc\n");
    }

    #[test]
    fn empty_line_is_accumulated() {
        let mut acc = OutputAccumulator::new(1024, 100);
        acc.push_line("");
        acc.push_line("x");
        assert_eq!(acc.to_string_lossy(), "\nx\n");
    }
}
