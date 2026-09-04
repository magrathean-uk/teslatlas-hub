// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

enum HubMetrics {
    static let windowSize = NSSize(width: 900, height: 630)
    static let referenceScale: CGFloat = 900.0 / 1040.0
    static let titlebarHeight: CGFloat = 38
    static let navigationHeight: CGFloat = 46
    static let cardRadius: CGFloat = 12
    static let controlRadius: CGFloat = 8
    static let sheetRadius: CGFloat = 14
    static let onboardingSheetSize = NSSize(width: 485, height: 350)
    static let welcomeSheetSize = NSSize(width: 485, height: 282)
    static let diagnosticsSheetSize = NSSize(width: 485, height: 422)
    static let logsSheetSize = NSSize(width: 554, height: 221)
    static let serviceDetailsSheetSize = NSSize(width: 450, height: 410)
    static let modalHeaderHeight: CGFloat = 38
    static let modalFooterHeight: CGFloat = 48
    static let contentWidth: CGFloat = 588
    static let pageInset: CGFloat = 21
    static let sectionSpacing: CGFloat = 12
    static let compactControlHeight: CGFloat = 28
}

enum HubPalette {
    private static func color(_ hex: UInt32, alpha: CGFloat = 1) -> NSColor {
        NSColor(
            srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: alpha
        )
    }

    private static func dynamic(light: NSColor, dark: NSColor) -> NSColor {
        NSColor(name: nil, dynamicProvider: { appearance in
            appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? dark : light
        })
    }

    static var foreground: NSColor {
        dynamic(light: color(0x1D1D1F), dark: color(0xF5F5F7))
    }

    static var background: NSColor {
        dynamic(light: color(0xFFFFFF), dark: color(0x1E1E1E))
    }

    static var mutedForeground: NSColor {
        dynamic(light: color(0x86868B), dark: color(0x98989D))
    }

    static var card: NSColor {
        dynamic(light: color(0xFFFFFF), dark: color(0x262628))
    }

    static var elevated: NSColor {
        dynamic(light: color(0xF5F5F7), dark: color(0x2C2C2E))
    }

    static var chrome: NSColor {
        dynamic(light: color(0xF6F6F8, alpha: 0.94),
                dark: color(0x2E2E30, alpha: 0.94))
    }

    static var chromeForeground: NSColor {
        dynamic(light: color(0x3A3A3C), dark: color(0xD1D1D6))
    }

    static var navigationGroup: NSColor {
        dynamic(light: color(0x000000, alpha: 0.04),
                dark: color(0xFFFFFF, alpha: 0.06))
    }

    static var hairline: NSColor {
        dynamic(light: color(0x000000, alpha: 0.08),
                dark: color(0xFFFFFF, alpha: 0.09))
    }

    static var border: NSColor {
        dynamic(light: color(0x000000, alpha: 0.12),
                dark: color(0xFFFFFF, alpha: 0.14))
    }

    static var accent: NSColor {
        dynamic(light: color(0x007AFF), dark: color(0x0A84FF))
    }

    static var success: NSColor {
        dynamic(light: color(0x34C759), dark: color(0x30D158))
    }

    static var danger: NSColor {
        dynamic(light: color(0xFF3B30), dark: color(0xFF453A))
    }

    static var warning: NSColor {
        dynamic(light: color(0xFF9500), dark: color(0xFF9F0A))
    }
}

enum HubButtonStyle: Equatable {
    case primary
    case neutral
    case flat
    case flatAccent
    case flatDanger
    case destructive
}

final class HubActionButton: NSButton {
    var hubFont = NSFont.systemFont(ofSize: 13, weight: .medium) {
        didSet {
            invalidateIntrinsicContentSize()
            updateHubAppearance()
        }
    }

    var hubStyle: HubButtonStyle = .neutral {
        didSet {
            invalidateIntrinsicContentSize()
            updateHubAppearance()
        }
    }

    override var isEnabled: Bool {
        didSet { updateHubAppearance() }
    }

    override var title: String {
        didSet {
            invalidateIntrinsicContentSize()
            updateHubAppearance()
        }
    }

    override var image: NSImage? {
        didSet { invalidateIntrinsicContentSize() }
    }

    override var intrinsicContentSize: NSSize {
        let measured = super.intrinsicContentSize
        let horizontalPadding: CGFloat
        switch hubStyle {
        case .flat, .flatAccent, .flatDanger:
            horizontalPadding = 12
        case .primary, .neutral, .destructive:
            horizontalPadding = 20
        }
        return NSSize(
            width: ceil(measured.width + horizontalPadding),
            height: max(HubMetrics.compactControlHeight, ceil(measured.height + 6))
        )
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateHubAppearance()
    }

    func updateHubAppearance() {
        wantsLayer = true
        isBordered = false
        alignment = .center
        cell?.lineBreakMode = .byClipping
        layer?.cornerRadius = HubMetrics.controlRadius
        layer?.cornerCurve = .continuous
        layer?.borderWidth = hubStyle == .neutral ? 1 : 0
        layer?.borderColor = HubPalette.border.cgColor
        layer?.shadowOpacity = hubStyle == .neutral ? 0.10 : 0
        layer?.shadowRadius = hubStyle == .neutral ? 1.5 : 0
        layer?.shadowOffset = NSSize(width: 0, height: -1)

        let foreground: NSColor
        switch hubStyle {
        case .primary:
            layer?.backgroundColor = (isEnabled ? HubPalette.accent : .disabledControlTextColor).cgColor
            foreground = .white
        case .destructive:
            layer?.backgroundColor = (isEnabled ? HubPalette.danger : .disabledControlTextColor).cgColor
            foreground = .white
        case .neutral:
            layer?.backgroundColor = HubPalette.card.cgColor
            foreground = isEnabled ? HubPalette.foreground : .disabledControlTextColor
        case .flat:
            layer?.backgroundColor = NSColor.clear.cgColor
            foreground = isEnabled ? HubPalette.foreground : .disabledControlTextColor
        case .flatAccent:
            layer?.backgroundColor = NSColor.clear.cgColor
            foreground = isEnabled ? HubPalette.accent : .disabledControlTextColor
        case .flatDanger:
            layer?.backgroundColor = NSColor.clear.cgColor
            foreground = isEnabled ? HubPalette.danger : .disabledControlTextColor
        }

        contentTintColor = foreground
        attributedTitle = NSAttributedString(
            string: title,
            attributes: [
                .foregroundColor: foreground,
                .font: hubFont
            ]
        )
        toolTip = title.isEmpty ? toolTip : title
    }
}

enum HubSurfaceFill {
    case background
    case card
    case elevated
    case chrome
    case navigationGroup

    var color: NSColor {
        switch self {
        case .background: return HubPalette.background
        case .card: return HubPalette.card
        case .elevated: return HubPalette.elevated
        case .chrome: return HubPalette.chrome
        case .navigationGroup: return HubPalette.navigationGroup
        }
    }
}

class HubSurfaceView: NSView {
    var fill: HubSurfaceFill {
        didSet { updateLayer() }
    }

    init(fill: HubSurfaceFill = .background) {
        self.fill = fill
        super.init(frame: .zero)
        wantsLayer = true
        updateLayer()
    }

    override func updateLayer() {
        layer?.backgroundColor = fill.color.cgColor
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateLayer()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

/// Scroll-view document surfaces use a top-left origin so their first row is
/// visible when the sheet opens, matching the rest of the application layout.
final class HubFlippedSurfaceView: HubSurfaceView {
    override var isFlipped: Bool { true }
}

final class HubCardView: NSView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.cornerRadius = HubMetrics.cardRadius
        layer?.cornerCurve = .continuous
        layer?.borderWidth = 0.5
        layer?.shadowColor = NSColor.black.cgColor
        layer?.shadowOpacity = 0.04
        layer?.shadowRadius = 2
        layer?.shadowOffset = NSSize(width: 0, height: -1)
        updateLayer()
    }

    override func updateLayer() {
        layer?.backgroundColor = HubPalette.card.cgColor
        layer?.borderColor = HubPalette.hairline.cgColor
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateLayer()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

enum HubStatusTone {
    case neutral
    case success
    case warning
    case danger

    var color: NSColor {
        switch self {
        case .neutral: return HubPalette.mutedForeground
        case .success: return HubPalette.success
        case .warning: return HubPalette.warning
        case .danger: return HubPalette.danger
        }
    }
}

final class HubStatusRowView: NSView {
    private let valueLabel = NSTextField(labelWithString: "")
    private let statusDot = NSView()

    var value: String {
        get { valueLabel.stringValue }
        set { valueLabel.stringValue = newValue }
    }

    var statusTone: HubStatusTone? {
        didSet { updateStatusDot() }
    }

    init(symbol: String, title: String) {
        super.init(frame: .zero)
        let icon = NSImageView(image: NSImage(systemSymbolName: symbol,
                                              accessibilityDescription: nil) ?? NSImage())
        let titleLabel = NSTextField(labelWithString: title)
        icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 14, weight: .regular)
        icon.contentTintColor = HubPalette.mutedForeground
        icon.widthAnchor.constraint(equalToConstant: 17).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 17).isActive = true
        titleLabel.font = .systemFont(ofSize: 11.5, weight: .medium)
        titleLabel.textColor = HubPalette.foreground
        valueLabel.font = .systemFont(ofSize: 11.5)
        valueLabel.textColor = HubPalette.mutedForeground
        valueLabel.lineBreakMode = .byTruncatingMiddle
        statusDot.wantsLayer = true
        statusDot.layer?.cornerRadius = 4
        statusDot.layer?.cornerCurve = .continuous
        statusDot.isHidden = true
        statusDot.widthAnchor.constraint(equalToConstant: 8).isActive = true
        statusDot.heightAnchor.constraint(equalToConstant: 8).isActive = true

        let stack = NSStackView(views: [icon, titleLabel, NSView(), statusDot, valueLabel])
        stack.translatesAutoresizingMaskIntoConstraints = false
        stack.alignment = .centerY
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -14),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 9),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -9)
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    private func updateStatusDot() {
        statusDot.isHidden = statusTone == nil
        statusDot.layer?.backgroundColor = statusTone?.color.cgColor
    }
}

enum HubSheetStyle {
    static func makeWindow(contentSize: NSSize) -> NSWindow {
        let window = NSWindow(contentRect: NSRect(origin: .zero, size: contentSize),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.backgroundColor = .clear
        window.isMovable = false
        window.styleMask.insert(.fullSizeContentView)
        window.standardWindowButton(.closeButton)?.isHidden = true
        window.standardWindowButton(.miniaturizeButton)?.isHidden = true
        window.standardWindowButton(.zoomButton)?.isHidden = true
        window.isOpaque = false
        return window
    }

    static func inset(_ view: NSView, horizontal: CGFloat, vertical: CGFloat) -> NSView {
        let root = HubCardView()
        view.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(view)
        NSLayoutConstraint.activate([
            view.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: horizontal),
            view.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -horizontal),
            view.topAnchor.constraint(equalTo: root.topAnchor, constant: vertical),
            view.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -vertical)
        ])
        return root
    }
}

enum HubAppearanceMode: String, Equatable {
    case system
    case light
    case dark
}

struct HubAppearancePreference {
    private let defaults: UserDefaults
    private let key: String
    private(set) var mode: HubAppearanceMode

    init(defaults: UserDefaults = .standard, key: String = "TeslatlasHubAppearance") {
        self.defaults = defaults
        self.key = key
        mode = defaults.string(forKey: key).flatMap(HubAppearanceMode.init(rawValue:)) ?? .system
    }

    @discardableResult
    mutating func toggle(currentIsDark: Bool) -> HubAppearanceMode {
        mode = currentIsDark ? .light : .dark
        defaults.set(mode.rawValue, forKey: key)
        return mode
    }

    func apply(to window: NSWindow) {
        switch mode {
        case .system: window.appearance = nil
        case .light: window.appearance = NSAppearance(named: .aqua)
        case .dark: window.appearance = NSAppearance(named: .darkAqua)
        }
    }
}
