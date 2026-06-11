import XCTest
@testable import HebIsland

final class ProtocolTests: XCTestCase {

    // MARK: - IncomingMessage decoding

    func testDecodeShowApproval() throws {
        let json = #"{"type":"show","id":"p1","card":{"id":"p1","cardType":"approval","title":"审批","body":"运行 cargo check","sessionId":"s1"}}"#
        let msg = try JSONDecoder().decode(IncomingMessage.self, from: json.data(using: .utf8)!)
        XCTAssertEqual(msg.type, "show")
        XCTAssertEqual(msg.id, "p1")
        XCTAssertEqual(msg.card?.id, "p1")
        XCTAssertEqual(msg.card?.cardType, "approval")
        XCTAssertEqual(msg.card?.title, "审批")
        XCTAssertEqual(msg.card?.body, "运行 cargo check")
        XCTAssertEqual(msg.card?.sessionId, "s1")
        XCTAssertNil(msg.card?.durationMs)
        XCTAssertNil(msg.card?.actions)
    }

    func testDecodeShowInfo() throws {
        let json = #"{"type":"show","id":"i1","card":{"id":"i1","cardType":"info","title":"完成","body":"编译通过"}}"#
        let msg = try JSONDecoder().decode(IncomingMessage.self, from: json.data(using: .utf8)!)
        XCTAssertEqual(msg.card?.cardType, "info")
    }

    func testDecodeShowWithDurationAndActions() throws {
        let json = #"{"type":"show","id":"c1","card":{"id":"c1","cardType":"approval","title":"T","body":"B","durationMs":3000,"actions":["知道了","忽略"]}}"#
        let msg = try JSONDecoder().decode(IncomingMessage.self, from: json.data(using: .utf8)!)
        XCTAssertEqual(msg.card?.durationMs, 3000)
        XCTAssertEqual(msg.card?.actions, ["知道了", "忽略"])
    }

    func testDecodeDismiss() throws {
        let json = #"{"type":"dismiss","id":"d1"}"#
        let msg = try JSONDecoder().decode(IncomingMessage.self, from: json.data(using: .utf8)!)
        XCTAssertEqual(msg.type, "dismiss")
        XCTAssertEqual(msg.id, "d1")
        XCTAssertNil(msg.card)
    }

    // MARK: - ActionMessage encoding

    func testEncodeActionMessage() throws {
        let msg = ActionMessage(msgId: "p1", result: ActionResult(action: "allow"))
        let jsonLine = msg.toJSONLine()!
        XCTAssertTrue(jsonLine.contains("\"msg_id\":\"p1\""))
        XCTAssertTrue(jsonLine.contains("\"action\":\"allow\""))
        // Must end with newline
        XCTAssertTrue(jsonLine.hasSuffix("\n"))
    }

    /// 单选答案的 wire 形态须对齐 protocol::UserAnswer::Selected。
    func testEncodeSingleSelectedAnswer() throws {
        let result = ActionResult(action: "submit", answer: .single(.selected(label: "右上角")))
        let json = ActionMessage(msgId: "question-1", result: result).toJSONLine()!
        XCTAssertTrue(json.contains("\"action\":\"submit\""))
        XCTAssertTrue(json.contains("\"type\":\"selected\""))
        XCTAssertTrue(json.contains("\"label\":\"右上角\""))
    }

    /// 多题答案须对齐 protocol::UserAnswer::Multi { items: [{title, answer}] }。
    func testEncodeMultiAnswer() throws {
        let items = [
            MultiAnswerItem(title: "策略", answer: .selected(label: "A")),
            MultiAnswerItem(title: "范围", answer: .selectedMulti(labels: ["x", "y"])),
            MultiAnswerItem(title: "备注", answer: .custom(text: "随便写写")),
        ]
        let result = ActionResult(action: "submit", answer: .multi(items: items))
        let json = ActionMessage(msgId: "question-2", result: result).toJSONLine()!
        XCTAssertTrue(json.contains("\"type\":\"multi\""))
        XCTAssertTrue(json.contains("\"title\":\"策略\""))
        XCTAssertTrue(json.contains("\"type\":\"selected_multi\""))
        XCTAssertTrue(json.contains("\"type\":\"custom\""))
    }

    /// 多题卡解码：questions 数组应被吃下。
    func testDecodeMultiQuestionCard() throws {
        let json = #"{"type":"show","id":"q1","card":{"id":"q1","cardType":"question","title":"T","body":"","questions":[{"title":"策略","options":[{"label":"A"},{"label":"B"}],"multi":false},{"title":"范围","options":[{"label":"x"}],"multi":true}]}}"#
        let msg = try JSONDecoder().decode(IncomingMessage.self, from: json.data(using: .utf8)!)
        XCTAssertEqual(msg.card?.isMultiQuestion, true)
        XCTAssertEqual(msg.card?.multiQuestions.count, 2)
        XCTAssertEqual(msg.card?.multiQuestions[1].isMulti, true)
    }

    // MARK: - NotificationCard defaults

    func testInfoDefaults() {
        let card = NotificationCard(id: "i1", cardType: "info", title: "T", body: "B", sessionId: nil, durationMs: nil, actions: nil)
        XCTAssertEqual(card.effectiveDurationMs, 5000)
        XCTAssertTrue(card.resolvedButtons.isEmpty)
    }

    func testApprovalDefaults() {
        let card = NotificationCard(id: "a1", cardType: "approval", title: "T", body: "B", sessionId: nil, durationMs: nil, actions: nil)
        XCTAssertNil(card.effectiveDurationMs)
        let buttons = card.resolvedButtons
        XCTAssertEqual(buttons.count, 5)
        XCTAssertEqual(buttons[0].label, "拒绝")
        XCTAssertEqual(buttons[0].action, "deny")
        XCTAssertEqual(buttons[1].label, "一次")
        XCTAssertEqual(buttons[1].action, "allow")
        XCTAssertEqual(buttons[4].label, "全局")
        XCTAssertEqual(buttons[4].action, "allow_global")
    }

    func testQuestionDefaults() {
        let card = NotificationCard(id: "q1", cardType: "question", title: "T", body: "B", sessionId: nil, durationMs: nil, actions: nil)
        XCTAssertNil(card.effectiveDurationMs)
        // question 走专门的选项/输入 UI，不走通用按钮行。
        XCTAssertTrue(card.resolvedButtons.isEmpty)
    }

    func testZeroDurationMeansNever() {
        let card = NotificationCard(id: "i2", cardType: "info", title: "T", body: "B", sessionId: nil, durationMs: 0, actions: nil)
        XCTAssertNil(card.effectiveDurationMs)
    }

    func testCustomDuration() {
        let card = NotificationCard(id: "i3", cardType: "info", title: "T", body: "B", sessionId: nil, durationMs: 10000, actions: nil)
        XCTAssertEqual(card.effectiveDurationMs, 10000)
    }

    func testCustomActionsOverrideDefaults() {
        let card = NotificationCard(id: "a2", cardType: "approval", title: "T", body: "B", sessionId: nil, durationMs: nil, actions: ["接受", "拒绝"])
        let buttons = card.resolvedButtons
        XCTAssertEqual(buttons.count, 2)
        XCTAssertEqual(buttons[0].label, "接受")
        XCTAssertEqual(buttons[0].action, "接受")
        XCTAssertEqual(buttons[1].label, "拒绝")
        XCTAssertEqual(buttons[1].action, "拒绝")
    }
}
