//! 网络重试策略。
//!
//! 集中定义“哪些失败值得重试”与退避节奏，供元数据抓取与内容下载复用。

use std::io;
use std::time::Duration;

/// 单次请求的最大尝试次数（含首次）。
pub(crate) const ATTEMPTS: u32 = 4;

/// HTTP 状态码是否值得重试。
///
/// 4xx 中只有超时、过早请求与限流可重试；5xx 一律视为暂时故障。
pub(crate) fn retryable_status(code: u16) -> bool {
    matches!(code, 408 | 425 | 429) || (500..=599).contains(&code)
}

/// I/O 错误是否值得重试。
pub(crate) fn retryable_io(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::TimedOut
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
    )
}

/// 第 `attempt` 次失败后的等待时间：250ms 起倍增，带 ±20% 抖动，上限 4s。
pub(crate) fn backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(4);
    let base = 250_u64.saturating_mul(1_u64 << exponent);
    let jitter = base / 5;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| u64::from(duration.subsec_nanos()));
    let offset = if jitter == 0 {
        0
    } else {
        seed % (jitter * 2 + 1)
    };
    Duration::from_millis(base.saturating_sub(jitter).saturating_add(offset))
}

#[cfg(test)]
mod tests {
    use super::{backoff, retryable_io, retryable_status};
    use std::io;
    use std::time::Duration;

    #[test]
    fn classifies_status_codes() {
        assert!(retryable_status(429));
        assert!(retryable_status(503));
        assert!(!retryable_status(404));
        assert!(!retryable_status(403));
        assert!(!retryable_status(200));
    }

    #[test]
    fn classifies_io_kinds() {
        assert!(retryable_io(io::ErrorKind::TimedOut));
        assert!(retryable_io(io::ErrorKind::ConnectionReset));
        assert!(!retryable_io(io::ErrorKind::NotFound));
        assert!(!retryable_io(io::ErrorKind::PermissionDenied));
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert!(backoff(1) < Duration::from_millis(400));
        assert!(backoff(2) > Duration::from_millis(300));
        assert!(backoff(8) <= Duration::from_millis(4800));
    }
}
