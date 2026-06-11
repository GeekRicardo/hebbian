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

/// 一道子题（多题 question 卡）。单题卡用顶层 options/multiSelect，多题卡用本结构数组。
struct CardQuestion: Decodable {
    let title: String
    let desc: String?
    let options: [CardOption]
    let multi: Bool?

    var isMulti: Bool { multi ?? false }
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
    var options: [CardOption]? = nil      // 单题 question 卡的可选项
    var multiSelect: Bool? = nil          // 单题 question 卡是否多选（默认单选）
    var questions: [CardQuestion]? = nil  // 多题 question 卡：非空时逐题渲染，body/options 忽略
    var subcommands: [CardSubcommand]? = nil // approval 卡的子命令勾选列表

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

    /// 多题卡：非空 questions。否则按单题卡处理。
    var multiQuestions: [CardQuestion] { questions ?? [] }
    var isMultiQuestion: Bool { !(questions ?? []).isEmpty }
    var isMultiSelect: Bool { multiSelect ?? false }
}

// MARK: - 问答答案（与 Rust 端 protocol::UserAnswer wire 形态逐字对齐）

/// 一道题的单题答案。`type` + 字段对齐 protocol::SingleAnswer / UserAnswer 的非 Multi 分支。
enum SingleAnswer: Encodable {
    case selected(label: String)
    case selectedMulti(labels: [String])
    case custom(text: String)
    case cancelled

    enum CodingKeys: String, CodingKey { case type, label, labels, text }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .selected(let label):
            try c.encode("selected", forKey: .type)
            try c.encode(label, forKey: .label)
        case .selectedMulti(let labels):
            try c.encode("selected_multi", forKey: .type)
            try c.encode(labels, forKey: .labels)
        case .custom(let text):
            try c.encode("custom", forKey: .type)
            try c.encode(text, forKey: .text)
        case .cancelled:
            try c.encode("cancelled", forKey: .type)
        }
    }
}

/// 多题答案的一项：题目标题 + 子答案（对齐 protocol::MultiQuestionAnswer）。
struct MultiAnswerItem: Encodable {
    let title: String
    let answer: SingleAnswer
}

/// 一轮 ask 的完整答案，提交时整体序列化进回传的 `answer` 字段。
/// 与 protocol::UserAnswer 同形：单题走 SingleAnswer 的四个分支，多题走 .multi。
enum UserAnswer: Encodable {
    case single(SingleAnswer)
    case multi(items: [MultiAnswerItem])

    enum MultiKeys: String, CodingKey { case type, items }

    func encode(to encoder: Encoder) throws {
        switch self {
        case .single(let s):
            try s.encode(to: encoder)
        case .multi(let items):
            var c = encoder.container(keyedBy: MultiKeys.self)
            try c.encode("multi", forKey: .type)
            try c.encode(items, forKey: .items)
        }
    }
}

/// 用户操作结果：按钮 action + 可选的问答答案 / 子命令勾选。
struct ActionResult {
    let action: String
    var answer: UserAnswer? = nil   // question 提交时的完整答案
    var checked: [Int]? = nil       // approval 勾选的子命令索引
}

/// Outgoing action message from island to caller
struct ActionMessage: Encodable {
    let msgId: String
    let action: String
    let answer: UserAnswer?
    let checked: [Int]?

    enum CodingKeys: String, CodingKey {
        case msgId = "msg_id"
        case action, answer, checked
    }

    init(msgId: String, result: ActionResult) {
        self.msgId = msgId
        self.action = result.action
        self.answer = result.answer
        self.checked = result.checked
    }

    /// Serialize as single-line JSON with newline terminator
    func toJSONLine() -> String? {
        guard let data = try? JSONEncoder().encode(self),
              let str = String(data: data, encoding: .utf8) else { return nil }
        return str.trimmingCharacters(in: .newlines) + "\n"
    }
}
