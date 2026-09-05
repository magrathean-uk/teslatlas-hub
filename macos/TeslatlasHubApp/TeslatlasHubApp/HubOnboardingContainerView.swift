// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

private final class HubOnboardingDocumentView: NSView {
    override var isFlipped: Bool { true }
}

final class HubOnboardingContainerView: NSView {
    let headerView: NSView
    let bodyScrollView = NSScrollView()
    let bodyDocumentView: NSView = HubOnboardingDocumentView()
    let footerView: NSView

    private let footerContentHost = NSView()
    private var body: NSView?
    private var footerContent: NSView?

    init(headerView: NSView = NSView(), footerView: NSView = NSView()) {
        self.headerView = headerView
        self.footerView = footerView
        super.init(frame: .zero)

        identifier = NSUserInterfaceItemIdentifier("onboarding.container")
        wantsLayer = true
        layer?.cornerRadius = HubMetrics.sheetRadius
        layer?.cornerCurve = .continuous
        layer?.masksToBounds = true

        for view in [headerView, bodyScrollView, footerView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }

        bodyScrollView.identifier = NSUserInterfaceItemIdentifier("onboarding.body-scroll")
        bodyScrollView.drawsBackground = false
        bodyScrollView.borderType = .noBorder
        bodyScrollView.hasHorizontalScroller = false
        bodyScrollView.hasVerticalScroller = true
        bodyScrollView.autohidesScrollers = true
        bodyScrollView.documentView = bodyDocumentView
        bodyDocumentView.translatesAutoresizingMaskIntoConstraints = false

        footerContentHost.translatesAutoresizingMaskIntoConstraints = false
        footerView.addSubview(footerContentHost)

        NSLayoutConstraint.activate([
            headerView.leadingAnchor.constraint(equalTo: leadingAnchor),
            headerView.trailingAnchor.constraint(equalTo: trailingAnchor),
            headerView.topAnchor.constraint(equalTo: topAnchor),
            headerView.heightAnchor.constraint(equalToConstant: 38),

            bodyScrollView.leadingAnchor.constraint(equalTo: leadingAnchor),
            bodyScrollView.trailingAnchor.constraint(equalTo: trailingAnchor),
            bodyScrollView.topAnchor.constraint(equalTo: headerView.bottomAnchor),
            bodyScrollView.bottomAnchor.constraint(equalTo: footerView.topAnchor),

            footerView.leadingAnchor.constraint(equalTo: leadingAnchor),
            footerView.trailingAnchor.constraint(equalTo: trailingAnchor),
            footerView.bottomAnchor.constraint(equalTo: bottomAnchor),
            footerView.heightAnchor.constraint(equalToConstant: 48),

            bodyDocumentView.leadingAnchor.constraint(equalTo: bodyScrollView.contentView.leadingAnchor),
            bodyDocumentView.trailingAnchor.constraint(equalTo: bodyScrollView.contentView.trailingAnchor),
            bodyDocumentView.topAnchor.constraint(equalTo: bodyScrollView.contentView.topAnchor),
            bodyDocumentView.widthAnchor.constraint(equalTo: bodyScrollView.contentView.widthAnchor),
            bodyDocumentView.heightAnchor.constraint(greaterThanOrEqualTo: bodyScrollView.contentView.heightAnchor),

            footerContentHost.leadingAnchor.constraint(equalTo: footerView.leadingAnchor, constant: 28),
            footerContentHost.trailingAnchor.constraint(equalTo: footerView.trailingAnchor, constant: -28),
            footerContentHost.topAnchor.constraint(equalTo: footerView.topAnchor),
            footerContentHost.bottomAnchor.constraint(equalTo: footerView.bottomAnchor)
        ])
    }

    func replaceBody(_ body: NSView) {
        self.body?.removeFromSuperview()
        self.body = body
        body.translatesAutoresizingMaskIntoConstraints = false
        bodyDocumentView.addSubview(body)
        NSLayoutConstraint.activate([
            body.leadingAnchor.constraint(equalTo: bodyDocumentView.leadingAnchor, constant: 28),
            body.trailingAnchor.constraint(equalTo: bodyDocumentView.trailingAnchor, constant: -28),
            body.topAnchor.constraint(equalTo: bodyDocumentView.topAnchor, constant: 24),
            body.bottomAnchor.constraint(equalTo: bodyDocumentView.bottomAnchor, constant: -16)
        ])
        scrollBodyToTop()
    }

    func replaceFooterContent(_ content: NSView) {
        footerContent?.removeFromSuperview()
        footerContent = content
        content.translatesAutoresizingMaskIntoConstraints = false
        footerContentHost.addSubview(content)
        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: footerContentHost.leadingAnchor),
            content.trailingAnchor.constraint(equalTo: footerContentHost.trailingAnchor),
            content.centerYAnchor.constraint(equalTo: footerContentHost.centerYAnchor)
        ])
    }

    func scrollBodyToTop() {
        layoutSubtreeIfNeeded()
        bodyScrollView.contentView.scroll(to: .zero)
        bodyScrollView.reflectScrolledClipView(bodyScrollView.contentView)
    }

    func reveal(_ view: NSView) {
        layoutSubtreeIfNeeded()
        let rect = view.convert(view.bounds, to: bodyDocumentView)
        bodyDocumentView.scrollToVisible(rect.insetBy(dx: 0, dy: -8))
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}
