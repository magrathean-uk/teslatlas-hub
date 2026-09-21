// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

enum HubTypography {
    static let heading = NSFont.systemFont(ofSize: 22, weight: .semibold)
    static let body = NSFont.systemFont(ofSize: 14)
    static let label = NSFont.systemFont(ofSize: 12, weight: .medium)
    static let action = NSFont.systemFont(ofSize: 13, weight: .medium)
    static let emphasis = NSFont.systemFont(ofSize: 14, weight: .medium)
    static let caption = NSFont.systemFont(ofSize: 12)
}

enum HubMotion {
    static var reduceMotion: Bool {
        ProcessInfo.processInfo.environment["TESLATLAS_HUB_REDUCE_MOTION"] == "1"
            || NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
    }
    static var enabled: Bool {
        !HubUIPresentation.isSilentTestHost
    }

    static func transition(_ view: NSView, forward: Bool? = nil) {
        guard enabled, view.window?.isVisible == true else { return }
        view.wantsLayer = true
        let reduceMotion = self.reduceMotion
        let animation = CATransition()
        animation.type = reduceMotion || forward == nil ? .fade : .push
        if !reduceMotion { animation.subtype = forward == false ? .fromLeft : .fromRight }
        animation.duration = reduceMotion ? 0.10 : (forward == nil ? 0.16 : 0.22)
        animation.timingFunction = CAMediaTimingFunction(name: .easeOut)
        view.layer?.add(animation, forKey: "hub.content-transition")
    }

    static func click(_ view: NSView) {
        guard enabled, view.window?.isVisible == true else { return }
        let animation = CABasicAnimation(keyPath: "opacity")
        animation.fromValue = 0.55
        animation.toValue = 1
        animation.duration = 0.12
        animation.timingFunction = CAMediaTimingFunction(name: .easeOut)
        view.layer?.add(animation, forKey: "hub.click")
    }

    /// Animate from the currently displayed value so rapid interactions never
    /// queue effects or jump back to an earlier state.
    static func color(_ layer: CALayer, to color: CGColor) {
        let previous = layer.presentation()?.backgroundColor ?? layer.backgroundColor
        guard previous != color else { return }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        layer.backgroundColor = color
        CATransaction.commit()
        guard enabled, let previous else { return }
        let animation = CABasicAnimation(keyPath: "backgroundColor")
        animation.fromValue = previous
        animation.toValue = color
        animation.duration = reduceMotion ? 0.08 : 0.14
        animation.timingFunction = CAMediaTimingFunction(name: .easeOut)
        layer.add(animation, forKey: "hub.color")
    }

    static func layout(_ view: NSView, changes: () -> Void) {
        view.layoutSubtreeIfNeeded()
        NSAnimationContext.runAnimationGroup { context in
            context.duration = enabled && view.window?.isVisible == true && !reduceMotion ? 0.22 : 0
            context.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
            context.allowsImplicitAnimation = true
            changes()
            view.layoutSubtreeIfNeeded()
        }
    }
}

enum HubMetrics {
    static let windowSize = NSSize(width: 960, height: 730)
    static let referenceScale: CGFloat = 1
    static let titlebarHeight: CGFloat = 38
    static let navigationHeight: CGFloat = 46
    static let cardRadius: CGFloat = 12
    static let controlRadius: CGFloat = 8
    static let sheetRadius: CGFloat = 14
    static let onboardingSheetSize = NSSize(width: 620, height: 350)
    static let welcomeSheetSize = NSSize(width: 620, height: 282)
    static let diagnosticsSheetSize = NSSize(width: 680, height: 520)
    static let logsSheetSize = NSSize(width: 760, height: 500)
    static let serviceDetailsSheetSize = NSSize(width: 580, height: 500)
    static let modalHeaderHeight: CGFloat = 38
    static let modalFooterHeight: CGFloat = 48
    static let contentWidth: CGFloat = 744
    static let pageInset: CGFloat = 28
    static let pageTopInset: CGFloat = 32
    static let sectionSpacing: CGFloat = 16
    static let compactControlHeight: CGFloat = 32
    static let actionMinimumWidth: CGFloat = 96
    static let rowHeight: CGFloat = 52
    static let rowHorizontalInset: CGFloat = 16
    static let rowIconWell: CGFloat = 24
    static let rowSymbol: CGFloat = 18
    static let onboardingContentWidth: CGFloat = 640
    static let formLabelWidth: CGFloat = 144
    static let onboardingPrimaryHeight: CGFloat = compactControlHeight
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
    case navigationSelected
}

final class HubActionButton: NSButton {
    private var hoverTrackingArea: NSTrackingArea?
    private var hovered = false
    private let feedbackLayer = CALayer()

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let hoverTrackingArea { removeTrackingArea(hoverTrackingArea) }
        let area = NSTrackingArea(rect: .zero,
                                 options: [.mouseEnteredAndExited, .activeInKeyWindow, .inVisibleRect],
                                 owner: self, userInfo: nil)
        addTrackingArea(area)
        hoverTrackingArea = area
    }

    override func mouseEntered(with event: NSEvent) { hovered = true; updateFeedback() }
    override func mouseExited(with event: NSEvent) { hovered = false; updateFeedback() }
    override func highlight(_ flag: Bool) { super.highlight(flag); updateFeedback() }

    private func updateFeedback() {
        let opacity: CGFloat = !isEnabled ? 0 : (isHighlighted ? 0.12 : (hovered ? 0.06 : 0))
        HubMotion.color(feedbackLayer, to: NSColor.labelColor.withAlphaComponent(opacity).cgColor)
    }

    override var focusRingMaskBounds: NSRect { bounds }
    override func drawFocusRingMask() {
        NSBezierPath(roundedRect: bounds, xRadius: HubMetrics.controlRadius,
                     yRadius: HubMetrics.controlRadius).fill()
    }
    override func sendAction(_ action: Selector?, to target: Any?) -> Bool {
        guard isEnabled else { return false }
        HubMotion.click(self)
        return super.sendAction(action, to: target)
    }
    // Constraints describe our painted bounds, not NSButtonCell's bezel/image
    // alignment rect (which varies with the selected SF Symbol).
    override var alignmentRectInsets: NSEdgeInsets { NSEdgeInsetsZero }
    // Own the visible geometry. NSButtonCell's imageAbove layout puts the image
    // against the bezel edge and does not include our painted control insets.
    private(set) var hubImageView = NSImageView()
    private(set) var hubTitleLabel = NSTextField(labelWithString: "")
    private var installedContent = false
    var horizontalInset: CGFloat = 12 { didSet { invalidateIntrinsicContentSize(); needsLayout = true } }
    var iconBoxSize: CGFloat = 16 { didSet { invalidateIntrinsicContentSize(); needsLayout = true } }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        installContent()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        installContent()
    }

    private func installContent() {
        guard !installedContent else { return }
        installedContent = true
        wantsLayer = true
        feedbackLayer.cornerRadius = HubMetrics.controlRadius
        layer?.addSublayer(feedbackLayer)
        hubImageView.imageScaling = .scaleProportionallyDown
        hubTitleLabel.alignment = .center
        hubTitleLabel.lineBreakMode = .byClipping
        hubTitleLabel.maximumNumberOfLines = 1
        addSubview(hubImageView)
        addSubview(hubTitleLabel)
        updateHubAppearance()
    }

    override var imagePosition: NSControl.ImagePosition {
        didSet { invalidateIntrinsicContentSize(); needsLayout = true }
    }

    override var symbolConfiguration: NSImage.SymbolConfiguration? {
        didSet { updateHubAppearance() }
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        guard !isHidden, bounds.contains(convert(point, from: superview)) else { return nil }
        return self
    }

    override func draw(_ dirtyRect: NSRect) {
        // The layer paints the surface; the two child views paint the contents.
        // Do not ask NSButtonCell to draw a second image/title pair.
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        feedbackLayer.frame = bounds
        CATransaction.commit()
        let showsImage = image != nil && imagePosition != .noImage
        let showsTitle = !title.isEmpty && imagePosition != .imageOnly
        hubImageView.isHidden = !showsImage
        hubTitleLabel.isHidden = !showsTitle
        let textSize = (title as NSString).size(withAttributes: [.font: hubFont])
        let labelHeight = ceil(textSize.height)
        // Use the cell's measured width, including its native text insets.
        let labelWidth = min(ceil(hubTitleLabel.cell?.cellSize.width ?? textSize.width),
                             max(0, bounds.width - horizontalInset * 2
                                 - (showsImage ? iconBoxSize + 6 : 0)))
        let icon = iconBoxSize
        if imagePosition == .imageAbove || imagePosition == .imageBelow {
            let groupHeight = icon + (showsTitle ? 4 + labelHeight : 0)
            let bottom = (bounds.height - groupHeight) / 2
            let imageFirst = imagePosition == .imageAbove
            let imageY = isFlipped == imageFirst ? bottom : bottom + (showsTitle ? labelHeight + 4 : 0)
            let titleY = isFlipped == imageFirst ? bottom + icon + 4 : bottom
            hubTitleLabel.frame = showsTitle
                ? NSRect(x: (bounds.width - labelWidth) / 2, y: titleY,
                         width: labelWidth, height: labelHeight)
                : .zero
            hubImageView.frame = showsImage
                ? NSRect(x: (bounds.width - icon) / 2, y: imageY,
                         width: icon, height: icon)
                : .zero
        } else {
            let gap: CGFloat = showsImage && showsTitle ? 6 : 0
            let groupWidth = (showsImage ? icon : 0) + gap + (showsTitle ? labelWidth : 0)
            let left = (bounds.width - groupWidth) / 2
            let trailingImage = imagePosition == .imageTrailing || imagePosition == .imageRight
            hubImageView.frame = showsImage
                ? NSRect(x: trailingImage ? left + labelWidth + gap : left,
                         y: (bounds.height - icon) / 2, width: icon, height: icon)
                : .zero
            hubTitleLabel.frame = showsTitle
                ? NSRect(x: left + (showsImage && !trailingImage ? icon + gap : 0),
                         y: (bounds.height - labelHeight) / 2,
                         width: labelWidth, height: labelHeight)
                : .zero
        }
    }
    var hubFont = NSFont.systemFont(ofSize: 13, weight: .medium) {
        didSet {
            invalidateIntrinsicContentSize()
            updateHubAppearance()
        }
    }

    var hubStyle: HubButtonStyle = .neutral {
        didSet {
            guard hubStyle != oldValue else { return }
            let previousColor = layer?.presentation()?.backgroundColor ?? layer?.backgroundColor
            invalidateIntrinsicContentSize()
            updateHubAppearance()
            if window?.isVisible == true, let layer, let previousColor, let color = layer.backgroundColor {
                layer.backgroundColor = previousColor
                HubMotion.color(layer, to: color)
            }
        }
    }

    override var isEnabled: Bool {
        didSet { updateHubAppearance(); updateFeedback() }
    }

    override var title: String {
        didSet {
            invalidateIntrinsicContentSize()
            updateHubAppearance()
        }
    }

    override var image: NSImage? {
        didSet { invalidateIntrinsicContentSize(); updateHubAppearance() }
    }

    override var intrinsicContentSize: NSSize {
        let textWidth = imagePosition == .imageOnly ? 0 : ceil(hubTitleLabel.cell?.cellSize.width ?? (title as NSString).size(withAttributes: [.font: hubFont]).width)
        let hasIcon = image != nil && imagePosition != .noImage
        if imagePosition == .imageOnly {
            return NSSize(width: HubMetrics.compactControlHeight, height: HubMetrics.compactControlHeight)
        }
        if imagePosition == .imageAbove || imagePosition == .imageBelow {
            return NSSize(width: max(textWidth, iconBoxSize) + horizontalInset * 2, height: 51)
        }
        return NSSize(width: max(HubMetrics.actionMinimumWidth,
                                textWidth + (hasIcon ? iconBoxSize + 6 : 0) + horizontalInset * 2),
                      height: HubMetrics.compactControlHeight)
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateHubAppearance()
    }

    func updateHubAppearance() {
        effectiveAppearance.performAsCurrentDrawingAppearance { [self] in
            updateResolvedHubAppearance()
        }
    }

    private func updateResolvedHubAppearance() {
        guard installedContent else { return }
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
            layer?.backgroundColor = (imagePosition == .imageAbove ? HubPalette.elevated : HubPalette.card).cgColor
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
        case .navigationSelected:
            layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.12).cgColor
            foreground = isEnabled ? HubPalette.foreground : .disabledControlTextColor
        }

        contentTintColor = foreground
        hubImageView.image = image
        hubImageView.symbolConfiguration = symbolConfiguration ?? NSImage.SymbolConfiguration(pointSize: 14, weight: .regular)
        hubImageView.contentTintColor = foreground
        hubTitleLabel.stringValue = title
        hubTitleLabel.font = hubFont
        hubTitleLabel.textColor = foreground
        needsLayout = true
        needsDisplay = true
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

/// A single icon geometry for headers and grouped rows. Every SF Symbol gets
/// an explicit square drawing area, so symbols with different intrinsic widths
/// cannot move the text column or appear to hang outside their background.
final class HubIconTileView: HubSurfaceView {
    let imageView: NSImageView

    init(symbol: String,
         accessibilityDescription: String?,
         size: CGFloat = HubMetrics.rowIconWell,
         symbolSize: CGFloat = HubMetrics.rowSymbol,
         weight: NSFont.Weight = .regular,
         fill: HubSurfaceFill = .clear,
         tint: NSColor = .labelColor,
         radius: CGFloat = 9) {
        imageView = NSImageView(image: NSImage(systemSymbolName: symbol,
                                                accessibilityDescription: accessibilityDescription) ?? NSImage())
        super.init(fill: fill)
        wantsLayer = true
        layer?.cornerRadius = radius
        layer?.cornerCurve = .continuous
        imageView.imageScaling = .scaleProportionallyDown
        imageView.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: symbolSize, weight: weight)
        imageView.contentTintColor = tint
        imageView.translatesAutoresizingMaskIntoConstraints = false
        addSubview(imageView)
        NSLayoutConstraint.activate([
            widthAnchor.constraint(equalToConstant: size),
            heightAnchor.constraint(equalToConstant: size),
            imageView.widthAnchor.constraint(equalToConstant: symbolSize),
            imageView.heightAnchor.constraint(equalToConstant: symbolSize),
            imageView.centerXAnchor.constraint(equalTo: centerXAnchor),
            imageView.centerYAnchor.constraint(equalTo: centerYAnchor)
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

enum HubSurfaceFill {
    case clear
    case background
    case card
    case elevated
    case chrome
    case navigationGroup

    var color: NSColor {
        switch self {
        case .clear: return .clear
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
        effectiveAppearance.performAsCurrentDrawingAppearance { [self] in
            layer?.backgroundColor = fill.color.cgColor
        }
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
    var isSelected = false { didSet { updateLayer() } }
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
        effectiveAppearance.performAsCurrentDrawingAppearance { [self] in
            layer?.backgroundColor = HubPalette.card.cgColor
            layer?.borderWidth = isSelected ? 1.5 : 0.5
            layer?.borderColor = (isSelected ? HubPalette.accent : HubPalette.hairline).cgColor
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateLayer()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

final class HubOnboardingHairlineView: NSView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        translatesAutoresizingMaskIntoConstraints = false
        setContentHuggingPriority(.required, for: .vertical)
        setContentCompressionResistancePriority(.required, for: .vertical)
        updateLayer()
    }

    override var intrinsicContentSize: NSSize {
        NSSize(width: NSView.noIntrinsicMetric, height: 1)
    }

    override func updateLayer() {
        effectiveAppearance.performAsCurrentDrawingAppearance { [self] in
            layer?.backgroundColor = HubPalette.hairline.cgColor
        }
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

class HubStatusRowView: NSView {
    private let valueLabel = NSTextField(labelWithString: "")
    private let detailLabel = NSTextField(labelWithString: "")
    private let statusDot = NSView()

    var value: String {
        get { valueLabel.stringValue }
        set { valueLabel.stringValue = newValue }
    }

    var statusTone: HubStatusTone? {
        didSet { updateStatusDot() }
    }

    var detail: String {
        get { detailLabel.stringValue }
        set {
            detailLabel.stringValue = newValue
            detailLabel.isHidden = newValue.isEmpty
        }
    }

    init(symbol: String, title: String, detail: String = "", showsChevron: Bool = false) {
        super.init(frame: .zero)
        self.detail = detail
        let tile = HubIconTileView(symbol: symbol, accessibilityDescription: title)
        let titleLabel = NSTextField(labelWithString: title)
        titleLabel.font = .systemFont(ofSize: 14, weight: .medium)
        titleLabel.textColor = HubPalette.foreground
        detailLabel.font = .systemFont(ofSize: 12)
        detailLabel.textColor = HubPalette.mutedForeground
        detailLabel.lineBreakMode = .byTruncatingTail
        detailLabel.isHidden = detail.isEmpty
        let copy = NSStackView(views: [titleLabel, detailLabel])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 2
        valueLabel.font = .systemFont(ofSize: 13)
        valueLabel.textColor = HubPalette.foreground
        valueLabel.lineBreakMode = .byTruncatingMiddle
        valueLabel.alignment = .right
        valueLabel.widthAnchor.constraint(lessThanOrEqualToConstant: 300).isActive = true
        valueLabel.setContentHuggingPriority(.required, for: .horizontal)
        statusDot.wantsLayer = true
        statusDot.layer?.cornerRadius = 4
        statusDot.layer?.cornerCurve = .continuous
        statusDot.isHidden = true
        statusDot.widthAnchor.constraint(equalToConstant: 8).isActive = true
        statusDot.heightAnchor.constraint(equalToConstant: 8).isActive = true

        let chevron = NSImageView(image: NSImage(systemSymbolName: "chevron.right",
                                                  accessibilityDescription: nil) ?? NSImage())
        chevron.contentTintColor = .tertiaryLabelColor
        chevron.isHidden = !showsChevron
        chevron.translatesAutoresizingMaskIntoConstraints = false
        let status = NSStackView(views: [statusDot, valueLabel])
        status.spacing = 6
        status.alignment = .centerY
        let stack = NSStackView(views: [tile, copy, NSView(), status, chevron])
        stack.translatesAutoresizingMaskIntoConstraints = false
        stack.alignment = .centerY
        stack.spacing = 12
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: HubMetrics.rowHorizontalInset),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -HubMetrics.rowHorizontalInset),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
            chevron.widthAnchor.constraint(equalToConstant: 10),
            chevron.heightAnchor.constraint(equalToConstant: 16),
            heightAnchor.constraint(equalToConstant: HubMetrics.rowHeight)
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
