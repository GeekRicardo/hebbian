use serde::{Deserialize, Serialize};

/// 后台记忆抽取写入的单条记忆（架构 §4.14）。
/// 随 [`crate::event::EventPayload::MemoryExtracted`] 发给 surface 渲染明细。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryWriteItem {
    /// 记忆 id，形如 `proj/architecture` / `global/lang-pref`。
    pub id: String,
    /// 一句话摘要（L0），surface 在展开区每行显示这个。
    pub summary: String,
    /// 作用域标签："project" | "global"，surface 据此显示徽章颜色。
    pub scope: String,
}
