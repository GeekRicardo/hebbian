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
        let msg = ActionMessage(msgId: "p1", action: "allow")
        let jsonLine = msg.toJSONLine()!
        XCTAssertTrue(jsonLine.contains("\"msg_id\":\"p1\""))
        XCTAssertTrue(jsonLine.contains("\"action\":\"allow\""))
        // Must end with newline
        XCTAssertTrue(jsonLine.hasSuffix("\n"))
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
        XCTAssertEqual(buttons.count, 3)
        XCTAssertEqual(buttons[0].label, "拒绝")
        XCTAssertEqual(buttons[0].action, "deny")
        XCTAssertEqual(buttons[1].label, "允许")
        XCTAssertEqual(buttons[1].action, "allow")
        XCTAssertEqual(buttons[2].label, "打开")
        XCTAssertEqual(buttons[2].action, "open")
    }

    func testQuestionDefaults() {
        let card = NotificationCard(id: "q1", cardType: "question", title: "T", body: "B", sessionId: nil, durationMs: nil, actions: nil)
        XCTAssertNil(card.effectiveDurationMs)
        let buttons = card.resolvedButtons
        XCTAssertEqual(buttons.count, 1)
        XCTAssertEqual(buttons[0].label, "打开处理")
        XCTAssertEqual(buttons[0].action, "open")
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
