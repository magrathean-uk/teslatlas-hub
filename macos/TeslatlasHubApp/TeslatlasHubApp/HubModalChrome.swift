// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

final class HubModalRootView: HubSurfaceView {
    override init(fill: HubSurfaceFill = .card) {
        super.init(fill: fill)
        layer?.cornerRadius = HubMetrics.sheetRadius
        layer?.cornerCurve = .continuous
        layer?.masksToBounds = true
        layer?.borderWidth = 0.5
        layer?.borderColor = HubPalette.hairline.cgColor
    }

    override func updateLayer() {
        super.updateLayer()
        layer?.borderColor = HubPalette.hairline.cgColor
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

enum HubModalChrome {
    static func header(title: String,
                       trailing: [NSView],
                       identifier: String? = nil) -> NSView {
        let surface = HubSurfaceView(fill: .chrome)
        if let identifier {
            surface.identifier = NSUserInterfaceItemIdentifier(identifier)
        }
        let titleLabel = NSTextField(labelWithString: title)
        titleLabel.font = .systemFont(ofSize: 13, weight: .semibold)
        titleLabel.textColor = HubPalette.foreground
        let actions = NSStackView(views: trailing)
        actions.spacing = 6
        actions.alignment = .centerY
        let content = NSStackView(views: [titleLabel, NSView(), actions])
        content.alignment = .centerY
        content.translatesAutoresizingMaskIntoConstraints = false
        surface.addSubview(content)
        let line = hairline()
        line.translatesAutoresizingMaskIntoConstraints = false
        surface.addSubview(line)
        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: surface.leadingAnchor, constant: 16),
            content.trailingAnchor.constraint(equalTo: surface.trailingAnchor, constant: -12),
            content.centerYAnchor.constraint(equalTo: surface.centerYAnchor),
            line.leadingAnchor.constraint(equalTo: surface.leadingAnchor),
            line.trailingAnchor.constraint(equalTo: surface.trailingAnchor),
            line.bottomAnchor.constraint(equalTo: surface.bottomAnchor),
            line.heightAnchor.constraint(equalToConstant: 1),
            surface.heightAnchor.constraint(equalToConstant: HubMetrics.modalHeaderHeight)
        ])
        return surface
    }

    static func hairline() -> NSView {
        let line = HubSurfaceView(fill: .navigationGroup)
        line.identifier = NSUserInterfaceItemIdentifier("hub.modal.hairline")
        return line
    }

    static func divider() -> NSView {
        let line = HubSurfaceView(fill: .navigationGroup)
        line.heightAnchor.constraint(equalToConstant: 1).isActive = true
        return line
    }
}
