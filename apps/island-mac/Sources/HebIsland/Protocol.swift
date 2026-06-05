import Foundation

/// Incoming message from caller to island
struct IncomingMessage: Decodable {
    let type: String       // "show" or "dismiss"
    let id: String?
    let card: NotificationCard?
}

/// 问答选项（question 卡）
struct CardOption: Decodable {
    let label: String
    let desc: String?
}

/// 审批子命令（approval 卡的待审批队列勾选项）
struct CardSubcommand: Decodable {
    let tool: String       // 工具名（Bash / Edit ...）
    let detail: String?    // 命令或文件路径
    let checked: Bool?     // 默认是否勾选
}

/// Notification card content
struct NotificationCard: Decodable {
    let id: String
    let cardType: String   // "info" | "approval" | "question" | "success"
    let title: String
    let body: String
    let sessionId: String?
    let durationMs: Int?   // null/omit = default per cardType; 0 = never
    let actions: [String]? // null/omit = default per cardType
    let options: [CardOption]?      // question 卡的可选项
    let multiSelect: Bool?          // question 卡是否多选（默认单选）
    let subcommands: [CardSubcommand]? // approval 卡的子命令勾选列表

    /// Resolved auto-dismiss duration in ms (nil = never auto-dismiss)
    var effectiveDurationMs: Int? {
        if let d = durationMs {
            return d == 0 ? nil : d
        }
        switch cardType {
        case "info", "success": return 5000
        case "approval", "question": return nil
        default: return 5000
        }
    }

    /// Display labels and their wire action values for default buttons.
    /// For custom actions (when `actions` is non-empty), label == action value.
    var resolvedButtons: [(label: String, action: String)] {
        if let custom = actions, !custom.isEmpty {
            return custom.map { (label: $0, action: $0) }
        }
        switch cardType {
        case "approval":
            // 对齐 design.html：拒绝 / 一次 / 对话 / 项目 / 全局（允许的不同粒度）。
            // 注意：Desktop 当前只认 allow/deny；allow_conversation/project/global 待 Desktop HITL 支持。
            return [
                ("拒绝", "deny"),
                ("一次", "allow"),
                ("对话", "allow_conversation"),
                ("项目", "allow_project"),
                ("全局", "allow_global"),
            ]
        case "question", "info", "success":
            // question 走专门的选项/输入 UI（不走通用按钮行）；info/success 无按钮。
            return []
        default:
            return []
        }
    }

    var isMultiSelect: Bool { multiSelect ?? false }
}

/// 用户操作结果：按钮 action + 可选的问答选择 / 输入 / 子命令勾选。
struct ActionResult {
    let action: String
    var selected: [Int]? = nil   // question 选中的选项索引
    var input: String? = nil     // question 自由输入文本
    var checked: [Int]? = nil    // approval 勾选的子命令索引
}

/// Outgoing action message from island to caller
struct ActionMessage: Encodable {
    let msgId: String
    let action: String
    let selected: [Int]?
    let input: String?
    let checked: [Int]?

    enum CodingKeys: String, CodingKey {
        case msgId = "msg_id"
        case action, selected, input, checked
    }

    init(msgId: String, result: ActionResult) {
        self.msgId = msgId
        self.action = result.action
        self.selected = result.selected
        self.input = result.input
        self.checked = result.checked
    }

    /// Serialize as single-line JSON with newline terminator
    func toJSONLine() -> String? {
        guard let data = try? JSONEncoder().encode(self),
              let str = String(data: data, encoding: .utf8) else { return nil }
        return str.trimmingCharacters(in: .newlines) + "\n"
    }
}
