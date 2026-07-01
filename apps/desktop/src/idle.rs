//! 用户系统空闲检测（架构 §7.5.1，2026-06-20）。
//!
//! 机主离开电脑时，主对话的审批/问题转发到微信。判据是「系统输入设备空闲时长」——
//! 即多久没有键鼠等任何 HID 事件，最贴近「人不在电脑前」。macOS 走 CoreGraphics 的
//! `CGEventSourceSecondsSinceLastEventType`，零额外依赖；其它平台暂不支持，返回 0。

/// 距离上一次任意 HID 输入事件过去了多少秒。无法测得时返回 0（视为活跃，不误转发）。
pub fn seconds_since_last_input() -> f64 {
    #[cfg(target_os = "macos")]
    {
        // kCGEventSourceStateHIDSystemState = 1；kCGAnyInputEventType = 0xFFFFFFFF。
        const HID_SYSTEM_STATE: u32 = 1;
        const ANY_INPUT_EVENT: u32 = 0xFFFF_FFFF;
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGEventSourceSecondsSinceLastEventType(state_id: u32, event_type: u32) -> f64;
        }
        // SAFETY: 纯只读系统调用，无指针参数。
        unsafe { CGEventSourceSecondsSinceLastEventType(HID_SYSTEM_STATE, ANY_INPUT_EVENT) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        0.0
    }
}

/// 系统是否已空闲达到阈值（分钟）。阈值 0 视为关闭转发。
pub fn is_idle_for(threshold_minutes: u32) -> bool {
    if threshold_minutes == 0 {
        return false;
    }
    seconds_since_last_input() >= (threshold_minutes as f64) * 60.0
}
