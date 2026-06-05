import SwiftUI
import AppKit

/// 从打包资源加载彩色图标 PNG（微笑 / 傲娇 / 调皮）。
func themeIcon(_ name: String) -> Image? {
    guard let url = Bundle.module.url(forResource: name, withExtension: "png"),
          let nsImage = NSImage(contentsOf: url) else { return nil }
    return Image(nsImage: nsImage)
}

/// Visual theme derived from cardType
enum CardTheme {
    case info
    case approval
    case question
    case success

    init(cardType: String) {
        switch cardType {
        case "approval": self = .approval
        case "question": self = .question
        case "success": self = .success
        default: self = .info
        }
    }

    /// SF Symbol 名（仅作图标资源缺失时的兜底）。
    var icon: String {
        switch self {
        case .info: return "sparkles"
        case .approval: return "bolt.fill"
        case .question: return "questionmark.circle.fill"
        case .success: return "checkmark.circle.fill"
        }
    }

    var titleColor: Color {
        switch self {
        case .info: return Color(red: 102/255, green: 179/255, blue: 255/255)
        case .approval: return Color(red: 255/255, green: 179/255, blue: 71/255)
        case .question: return Color(red: 102/255, green: 179/255, blue: 255/255)
        case .success: return Color(red: 77/255, green: 217/255, blue: 102/255)
        }
    }

    var iconColor: Color { titleColor }

    var shouldPulse: Bool {
        switch self {
        case .approval, .question: return true
        case .info, .success: return false
        }
    }

    var pulseBaseColor: Color {
        switch self {
        case .approval: return Color(red: 255/255, green: 160/255, blue: 50/255)
        case .question: return Color(red: 100/255, green: 180/255, blue: 255/255)
        case .info, .success: return .clear
        }
    }

    var foldIcon: String { icon }
    var foldIconColor: Color { titleColor }

    var iconAsset: String {
        switch self {
        case .info: return "icon-info"
        case .approval: return "icon-approval"
        case .question: return "icon-approval"   // 问题也用傲娇
        case .success: return "icon-success"
        }
    }
}

// design.html 配色
private enum Palette {
    static let green = Color(red: 77/255, green: 217/255, blue: 102/255)
    static let greenBg = Color(red: 41/255, green: 97/255, blue: 46/255)
    static let greenBorder = Color(red: 71/255, green: 158/255, blue: 82/255)
    static let redBg = Color(red: 115/255, green: 31/255, blue: 31/255)
    static let redBorder = Color(red: 179/255, green: 64/255, blue: 64/255)
    static let blueBg = Color(red: 36/255, green: 71/255, blue: 133/255)
    static let blueBorder = Color(red: 71/255, green: 122/255, blue: 209/255)
    static let amber = Color(red: 255/255, green: 179/255, blue: 71/255)
    static let cyan = Color(red: 102/255, green: 179/255, blue: 255/255)
    static let grayBg = Color(red: 64/255, green: 64/255, blue: 64/255)
    static let grayBorder = Color.white.opacity(0.28)
}

/// The expanded notification card (full content).
struct CardView: View {
    let card: NotificationCard
    let onResult: (ActionResult) -> Void
    let onClose: () -> Void
    let onFold: () -> Void
    let onHoverEnter: () -> Void
    let onHoverExit: () -> Void
    let theme: CardTheme

    @State private var isHovering = false
    @State private var checkedSubs: Set<Int>      // 审批勾选的子命令
    @State private var selectedOpts: Set<Int> = []// 问答选中的选项
    @State private var inputText = ""              // 问答自由输入

    init(card: NotificationCard,
         onResult: @escaping (ActionResult) -> Void,
         onClose: @escaping () -> Void,
         onFold: @escaping () -> Void = {},
         onHoverEnter: @escaping () -> Void = {},
         onHoverExit: @escaping () -> Void = {},
         theme: CardTheme? = nil) {
        self.card = card
        self.onResult = onResult
        self.onClose = onClose
        self.onFold = onFold
        self.onHoverEnter = onHoverEnter
        self.onHoverExit = onHoverExit
        self.theme = theme ?? CardTheme(cardType: card.cardType)
        // 子命令默认勾选状态
        let defaults = Set((card.subcommands ?? []).enumerated().compactMap {
            ($0.element.checked == true) ? $0.offset : nil
        })
        _checkedSubs = State(initialValue: defaults)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            topline

            if !card.body.isEmpty {
                Text(card.body)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundColor(Color.white.opacity(0.65))
                    .lineLimit(2)
                    .padding(.bottom, 4)
            }

            if card.cardType == "question" {
                questionContent
            } else {
                if let subs = card.subcommands, !subs.isEmpty {
                    subcommandList(subs)
                }
                if hasActions {
                    actionsRow.padding(.top, 10)
                }
            }
        }
        .padding(12)
        .frame(width: 420)
        .background(Color.black)
        .cornerRadius(16)
        .overlay(RoundedRectangle(cornerRadius: 16).strokeBorder(borderColor, lineWidth: 1.5))
        .overlay(alignment: .topTrailing) {
            HStack(spacing: 6) {
                controlButton("\u{2304}", action: onFold)
                controlButton("\u{2715}", action: onClose)
            }
            .padding(.top, 8)
            .padding(.trailing, 10)
            .opacity(isHovering ? 1 : 0)
        }
        .onHover { hovering in
            isHovering = hovering
            if hovering { onHoverEnter() } else { onHoverExit() }
        }
    }

    // MARK: - Topline

    private var topline: some View {
        HStack(spacing: 8) {
            iconView.frame(width: 22, height: 22)
            Text(card.title)
                .font(.system(size: 11, weight: .bold, design: .monospaced))
                .foregroundColor(theme.titleColor)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 0)
        }
        .padding(.trailing, 52)
        .padding(.bottom, 6)
    }

    private var iconView: some View {
        Group {
            if let icon = themeIcon(theme.iconAsset) {
                icon.resizable().interpolation(.high)
            } else {
                Image(systemName: theme.icon).foregroundColor(theme.iconColor)
            }
        }
    }

    private func controlButton(_ glyph: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(glyph)
                .font(.system(size: 14, weight: .bold))
                .foregroundColor(Color.white.opacity(0.6))
                .frame(width: 26, height: 26)
                .background(Color.white.opacity(0.08))
                .cornerRadius(6)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.white.opacity(0.14), lineWidth: 1))
        }
        .buttonStyle(.plain)
    }

    // MARK: - Approval: 子命令勾选列表

    private func subcommandList(_ subs: [CardSubcommand]) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("待审批队列")
                .font(.system(size: 9, weight: .semibold, design: .monospaced))
                .foregroundColor(Color.white.opacity(0.45))
                .padding(.top, 8)
            ForEach(Array(subs.enumerated()), id: \.offset) { idx, sub in
                let on = checkedSubs.contains(idx)
                Button {
                    if on { checkedSubs.remove(idx) } else { checkedSubs.insert(idx) }
                } label: {
                    HStack(alignment: .top, spacing: 8) {
                        ZStack {
                            RoundedRectangle(cornerRadius: 3)
                                .fill(on ? Palette.greenBg : Color.clear)
                            RoundedRectangle(cornerRadius: 3)
                                .stroke(on ? Palette.greenBorder : Color.white.opacity(0.25), lineWidth: 1)
                            if on {
                                Text("\u{2713}").font(.system(size: 9, weight: .bold)).foregroundColor(Palette.green)
                            }
                        }
                        .frame(width: 14, height: 14)
                        .padding(.top, 1)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(sub.tool)
                                .font(.system(size: 10, weight: .semibold, design: .monospaced))
                                .foregroundColor(Palette.amber)
                            if let d = sub.detail, !d.isEmpty {
                                Text(d)
                                    .font(.system(size: 9, design: .monospaced))
                                    .foregroundColor(Color.white.opacity(0.45))
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.vertical, 5)
                    .padding(.horizontal, 8)
                    .background(Color.white.opacity(0.02))
                    .cornerRadius(4)
                }
                .buttonStyle(.plain)
            }
        }
    }

    // MARK: - Approval: 按钮行

    private var hasActions: Bool { !card.resolvedButtons.isEmpty }

    private var actionsRow: some View {
        HStack(spacing: 6) {
            ForEach(Array(card.resolvedButtons.enumerated()), id: \.offset) { _, btn in
                Button {
                    let checked = checkedSubs.isEmpty ? nil : Array(checkedSubs).sorted()
                    onResult(ActionResult(action: btn.action, checked: checked))
                } label: {
                    Text(btn.label)
                        .font(.system(size: 10, weight: .semibold, design: .monospaced))
                        .foregroundColor(Color.white.opacity(0.95))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 7)
                        .background(buttonBackground(for: btn.action))
                        .cornerRadius(4)
                        .overlay(RoundedRectangle(cornerRadius: 4).stroke(buttonBorder(for: btn.action), lineWidth: 1))
                }
                .buttonStyle(.plain)
            }
        }
    }

    // MARK: - Question: 选项 + 输入 + 跳过/提交

    private var questionContent: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let opts = card.options, !opts.isEmpty {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array(opts.enumerated()), id: \.offset) { idx, opt in
                        optionRow(idx, opt)
                    }
                }
            }

            // 自由输入
            HStack(spacing: 6) {
                Text(">").font(.system(size: 10, weight: .bold, design: .monospaced)).foregroundColor(Palette.green)
                TextField("自由输入…", text: $inputText)
                    .textFieldStyle(.plain)
                    .font(.system(size: 10.5, design: .monospaced))
                    .foregroundColor(.white)
            }
            .padding(.vertical, 6).padding(.horizontal, 10)
            .background(Color.white.opacity(0.03))
            .overlay(RoundedRectangle(cornerRadius: 4).stroke(Color.white.opacity(0.10), lineWidth: 1))
            .cornerRadius(4)

            HStack(spacing: 6) {
                Button { onResult(ActionResult(action: "skip")) } label: {
                    Text("跳过")
                        .font(.system(size: 10, weight: .semibold, design: .monospaced))
                        .foregroundColor(Color.white.opacity(0.6))
                        .frame(maxWidth: .infinity).padding(.vertical, 7)
                        .background(Color.white.opacity(0.04))
                        .cornerRadius(4)
                        .overlay(RoundedRectangle(cornerRadius: 4).stroke(Color.white.opacity(0.10), lineWidth: 1))
                }.buttonStyle(.plain)

                Button {
                    let sel = selectedOpts.isEmpty ? nil : Array(selectedOpts).sorted()
                    let text = inputText.isEmpty ? nil : inputText
                    onResult(ActionResult(action: "submit", selected: sel, input: text))
                } label: {
                    Text("提交")
                        .font(.system(size: 10, weight: .semibold, design: .monospaced))
                        .foregroundColor(Color.white.opacity(0.95))
                        .frame(maxWidth: .infinity).padding(.vertical, 7)
                        .background(Palette.greenBg)
                        .cornerRadius(4)
                        .overlay(RoundedRectangle(cornerRadius: 4).stroke(Palette.greenBorder, lineWidth: 1))
                }.buttonStyle(.plain)
            }
        }
        .padding(.top, 2)
    }

    private func optionRow(_ idx: Int, _ opt: CardOption) -> some View {
        let on = selectedOpts.contains(idx)
        return Button {
            if card.isMultiSelect {
                if on { selectedOpts.remove(idx) } else { selectedOpts.insert(idx) }
            } else {
                selectedOpts = [idx]   // 单选
            }
        } label: {
            HStack(alignment: .top, spacing: 8) {
                // 多选用方框，单选用圆点
                ZStack {
                    if card.isMultiSelect {
                        RoundedRectangle(cornerRadius: 3).fill(on ? Palette.cyan.opacity(0.25) : .clear)
                        RoundedRectangle(cornerRadius: 3).stroke(on ? Palette.cyan : Color.white.opacity(0.25), lineWidth: 1)
                        if on { Text("\u{2713}").font(.system(size: 9, weight: .bold)).foregroundColor(Palette.cyan) }
                    } else {
                        Circle().stroke(on ? Palette.cyan : Color.white.opacity(0.25), lineWidth: 1)
                        if on { Circle().fill(Palette.cyan).frame(width: 7, height: 7) }
                    }
                }
                .frame(width: 14, height: 14)
                .padding(.top, 1)
                VStack(alignment: .leading, spacing: 2) {
                    Text(opt.label)
                        .font(.system(size: 10.5, design: .monospaced))
                        .foregroundColor(on ? .white : Color.white.opacity(0.75))
                    if let d = opt.desc, !d.isEmpty {
                        Text(d).font(.system(size: 9, design: .monospaced)).foregroundColor(Color.white.opacity(0.4))
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.vertical, 6).padding(.horizontal, 10)
            .background(Color.white.opacity(on ? 0.06 : 0.03))
            .cornerRadius(4)
        }
        .buttonStyle(.plain)
    }

    // MARK: - Colors

    private var borderColor: Color { theme.titleColor.opacity(0.55) }

    private func buttonBackground(for action: String) -> Color {
        switch action {
        case "deny": return Palette.redBg
        case "allow", "allow_conversation", "allow_project": return Palette.greenBg
        case "allow_global": return Palette.blueBg
        default: return Palette.grayBg
        }
    }

    private func buttonBorder(for action: String) -> Color {
        switch action {
        case "deny": return Palette.redBorder
        case "allow", "allow_conversation", "allow_project": return Palette.greenBorder
        case "allow_global": return Palette.blueBorder
        default: return Palette.grayBorder
        }
    }
}

/// Compact 48x48 folded card with a single theme-colored icon.
struct FoldedCardView: View {
    let theme: CardTheme
    let onTap: () -> Void
    let onHoverEnter: () -> Void
    let onHoverExit: () -> Void

    @State private var pulse: CGFloat = 0

    var body: some View {
        Group {
            if let icon = themeIcon(theme.iconAsset) {
                icon.resizable().interpolation(.high).frame(width: 30, height: 30)
            } else {
                Image(systemName: theme.foldIcon)
                    .font(.system(size: 20, weight: .bold))
                    .foregroundColor(theme.foldIconColor)
            }
        }
        .frame(width: 48, height: 48)
        .background(Color.black)
        .cornerRadius(18)
        .overlay(RoundedRectangle(cornerRadius: 18).strokeBorder(borderColor, lineWidth: 1.5))
        .contentShape(Rectangle())
        .onTapGesture { onTap() }
        .onHover { hovering in
            if hovering { onHoverEnter() } else { onHoverExit() }
        }
        .onAppear {
            withAnimation(.easeInOut(duration: 2).repeatForever(autoreverses: true)) {
                pulse = 1
            }
        }
    }

    private var borderColor: Color {
        theme.titleColor.opacity(0.3 + 0.5 * pulse)
    }
}
