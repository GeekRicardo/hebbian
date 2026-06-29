//! 深睡整合（架构 §4.14 / §3.1）。
//!
//! 浅睡（`memory_extract`）只把零散事实抽出来落盘；深睡在用户**不等待**的时段把它们
//! 「想」成有结构的记忆网络——去重整合、tag 归一、建关联边、升华洞察、遗忘衰减。
//!
//! 触发：① session 空闲 ≥ T 分钟（`WakeupScheduler` 的 idle 哨兵，实时）；② 回填脚本
//! 按历史两轮时间戳（离线）。两条路径共用 [`decide_sleep_depth`]——同一个空闲时长映射到
//! 同一套睡眠深度，不写两套逻辑。
//!
//! 睡得越久越深（呼应 sleep-time compute「睡得越久收益越大」）：空闲越长，跑的整合趟越多。

use crate::storage::memory::{mem_log, mem_warn};

/// 睡眠深度（架构 §3.1）：空闲时长决定跑几趟整合。趟是递增包含的——`Deep` 含 `Light`
/// 的全部，`Full` 含 `Deep` 的全部。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepDepth {
    /// 空闲不足 T：连续工作，不睡。
    None,
    /// ≥ T（默认 10min，喝杯水级）：去重整合 + tag 归一。
    Light,
    /// ≥ 1h（午饭 / 会议级）：加联结建边。
    Deep,
    /// 跨天（睡了一觉级）：加升华 + 遗忘衰减。
    Full,
}

impl SleepDepth {
    pub fn label(self) -> &'static str {
        match self {
            SleepDepth::None => "none",
            SleepDepth::Light => "light",
            SleepDepth::Deep => "deep",
            SleepDepth::Full => "full",
        }
    }
}

/// 把空闲时长（分钟）映射到睡眠深度（架构 §3.1）。实时 idle 哨兵与离线回填共用此函数。
///
/// `idle_threshold_min`：触发深睡的最小空闲（来自设置，默认 10min）。低于它不睡。
/// 分档阈值：≥ threshold → Light；≥ 60min → Deep；≥ 8h(480min) → Full。
pub fn decide_sleep_depth(idle_minutes: f64, idle_threshold_min: f64) -> SleepDepth {
    if idle_minutes < idle_threshold_min {
        SleepDepth::None
    } else if idle_minutes < 60.0 {
        SleepDepth::Light
    } else if idle_minutes < 480.0 {
        SleepDepth::Deep
    } else {
        SleepDepth::Full
    }
}

/// 深睡整合入口（架构 §4.14）。第 4 批填充真实的 N 趟（整合 / tag 归一 / 联结 / 升华 /
/// 遗忘）；当前为骨架：算出睡眠深度并记日志，验证 idle 触发链路打通。
///
/// `session_id` 仅用于日志定位；整合作用在 global + 当前 project 的记忆全集上。
pub async fn consolidate_for_session(
    _data_dir: &std::path::Path,
    session_id: &str,
    idle_minutes: f64,
    idle_threshold_min: f64,
) {
    let depth = decide_sleep_depth(idle_minutes, idle_threshold_min);
    if depth == SleepDepth::None {
        mem_warn!(
            "Sleep",
            "idle 触发但空闲不足（{idle_minutes:.1}min < {idle_threshold_min:.0}min）跳过 session={session_id}"
        );
        return;
    }
    // TODO(批4)：按 depth 跑 N 趟整合（整合 → tag 归一 → 联结 → 升华 → 遗忘）。
    mem_log!(
        "Sleep",
        "深睡触发 session={session_id} 空闲={idle_minutes:.1}min 深度={} （整合逻辑待批4）",
        depth.label()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_depth_thresholds() {
        let t = 10.0;
        assert_eq!(decide_sleep_depth(3.0, t), SleepDepth::None, "连续工作不睡");
        assert_eq!(decide_sleep_depth(10.0, t), SleepDepth::Light, "刚到 T → light");
        assert_eq!(decide_sleep_depth(45.0, t), SleepDepth::Light);
        assert_eq!(decide_sleep_depth(60.0, t), SleepDepth::Deep, "1h → deep");
        assert_eq!(decide_sleep_depth(300.0, t), SleepDepth::Deep);
        assert_eq!(decide_sleep_depth(480.0, t), SleepDepth::Full, "8h → full");
        assert_eq!(decide_sleep_depth(2000.0, t), SleepDepth::Full, "跨天 → full");
    }

    #[test]
    fn threshold_zero_means_always_light_above_zero() {
        // idle_threshold=0 时任何 >0 空闲都至少 light（边界：回填脚本可传 0 强制整理）。
        assert_eq!(decide_sleep_depth(0.0, 0.0), SleepDepth::Light);
    }
}
