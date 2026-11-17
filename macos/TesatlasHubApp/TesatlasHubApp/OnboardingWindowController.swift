// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

enum HubOnboardingPath: Equatable {
    case newInstallation
    case migration
}

enum HubOnboardingProvider: Equatable {
    case fleet
    case legacy
}

/// Whether the Fleet API card is offered (M09-T12). The build-time default stays off until the
/// Fleet set-up is proven in the lab; tests switch it on. The owner sign-in stays first and the
/// default whether or not Fleet is offered.
enum HubFleetOnboarding {
    static let offeredByDefault = false
    /// Read by the provider page, the Tesla account menu, Status and every route this window
    /// builds or renders. Only test code changes it.
    static var isOffered = offeredByDefault

    /// A state that names the Fleet route while Fleet is not offered falls back as the provider
    /// page does: to the owner sign-in. Applied to the preview environment, an explicit first
    /// route, navigation and every render, so no Fleet screen is built with the switch off.
    static func offered(_ state: HubOnboardingState) -> HubOnboardingState {
        guard state.route == .fleet || state.provider == .fleet, !isOffered else { return state }
        var fallback = state
        fallback.provider = .legacy
        if fallback.route == .fleet { fallback.route = .legacy }
        return fallback
    }
}

/// The Fleet route's steps, in the order of a first set-up (M09-T19a interface, section 10, with
/// DEC-222): the cost consent comes before the sign-in, because the redeem and `discover` list the
/// cars and Tesla bills that request. The last page shows the car and runs `setup-fleet activate`.
enum HubFleetStep: Equatable {
    case prerequisites
    case details
    case publicKey
    case consent
    case signIn
    case vehicles
    case start

    var previous: HubFleetStep? {
        switch self {
        case .prerequisites: return nil
        case .details: return .prerequisites
        case .publicKey: return .details
        case .consent, .signIn: return .publicKey
        case .vehicles, .start: return .signIn
        }
    }
}

enum HubOnboardingCompletion: Equatable {
    case configured
    case hubStarted
}

private enum HubOnboardingOperation {
    case importing
    case setup
}

private final class HubOnboardingChromeView: NSVisualEffectView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        material = .headerView
        blendingMode = .withinWindow
        state = .active
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

final class HubFormPopUpButton: NSPopUpButton {
    static let surfaceCornerRadius: CGFloat = 6

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect, pullsDown: false)
        configureSurface()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        configureSurface()
    }

    private func configureSurface() {
        isBordered = false
        controlSize = .regular
        font = HubTypography.body
        contentTintColor = HubPalette.foreground
        focusRingType = .default
    }

    override var alignmentRectInsets: NSEdgeInsets { NSEdgeInsetsZero }
    override var focusRingMaskBounds: NSRect { bounds }

    override func drawFocusRingMask() {
        NSBezierPath(roundedRect: bounds,
                     xRadius: Self.surfaceCornerRadius,
                     yRadius: Self.surfaceCornerRadius).fill()
    }

    override func draw(_ dirtyRect: NSRect) {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            let surface = bounds.insetBy(dx: 0.5, dy: 0.5)
            let path = NSBezierPath(roundedRect: surface,
                                    xRadius: Self.surfaceCornerRadius,
                                    yRadius: Self.surfaceCornerRadius)
            NSColor.textBackgroundColor.setFill()
            path.fill()
            HubPalette.border.setStroke()
            path.lineWidth = 1
            path.stroke()
        }
        super.draw(dirtyRect)
    }
}

enum HubOnboardingRoute: Equatable {
    case welcome
    case choose
    case provider
    case fleet
    case legacy
    case migration
    case verify
    case finish
}

struct HubOnboardingState: Equatable {
    var route: HubOnboardingRoute = .welcome
    var path: HubOnboardingPath = .newInstallation
    var provider: HubOnboardingProvider = .legacy

    var step: Int {
        switch route {
        case .welcome: return 1
        case .choose: return 2
        case .provider, .fleet, .legacy, .migration: return 3
        case .verify: return 4
        case .finish: return 5
        }
    }

    mutating func advance() {
        switch route {
        case .welcome:
            route = .choose
        case .choose:
            route = path == .newInstallation ? .provider : .migration
        case .provider:
            route = provider == .fleet && HubFleetOnboarding.isOffered ? .fleet : .legacy
        case .fleet, .legacy, .migration:
            route = .verify
        case .verify:
            route = .finish
        case .finish:
            break
        }
    }

    mutating func back() {
        switch route {
        case .choose:
            route = .welcome
        case .provider, .migration:
            route = .choose
        case .fleet, .legacy:
            route = .provider
        case .verify where path == .migration:
            route = .migration
        case .welcome, .verify, .finish:
            break
        }
    }
}

enum HubOnboardingDismissalPolicy: Equatable {
    case firstRun
    case accountManagement
}

/// The invisible control laid over a choice card. A native radio button's cell only tracks
/// its own radio and title, so a click on the card's empty space or icon often did nothing.
/// This one selects on a mouse-up that ends inside its whole bounds, on the first click even
/// when the window is not yet key, and keeps the radio's keyboard and accessibility behaviour.
final class HubChoiceRowButton: NSButton {
    private var pressStartedHere = false

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func hitTest(_ point: NSPoint) -> NSView? {
        guard !isHidden, frame.contains(point) else { return nil }
        return self
    }

    override func mouseDown(with event: NSEvent) {
        guard isEnabled else { return }
        pressStartedHere = true
    }

    override func mouseUp(with event: NSEvent) {
        defer { pressStartedHere = false }
        guard isEnabled, pressStartedHere,
              bounds.contains(convert(event.locationInWindow, from: nil)) else { return }
        withExtendedLifetime(self) { _ = sendAction(action, to: target) }
    }
}

final class OnboardingWindowController: NSWindowController, NSWindowDelegate, NSTextFieldDelegate {
    private let controller: HubController
    private let previewRoute: String?
    let dismissalPolicy: HubOnboardingDismissalPolicy
    private let onDismiss: () -> Void
    private let onComplete: (HubOnboardingCompletion) -> Void
    private let closesWindowOnCancel: Bool
    private var didNotifyDismiss = false
    private var state: HubOnboardingState
    private var busy = false
    private var busyMessage: String?
    private var busyOperation: BusyOperation?

    enum BusyOperation {
        case setup, importing, checkingCompatibility, connecting, runningChecks, starting
        case checkingSourceStopped, takingOver
        case fleet

        var message: String {
            switch self {
            case .setup: return HubL10n.text("hub.OnboardingWindowController.558.1914", fallback: "Setting up Hub…")
            case .importing: return HubL10n.text("hub.OnboardingWindowController.304.1870", fallback: "Importing data…")
            case .checkingCompatibility: return HubL10n.text("hub.OnboardingWindowController.busy_checking_compatibility", fallback: "Checking compatibility…")
            case .connecting: return HubL10n.text("hub.OnboardingWindowController.1323.2037", fallback: "Connecting…")
            case .runningChecks: return HubL10n.text("hub.DiagnosticsWindowController.210.227", fallback: "Running checks…")
            case .starting: return HubL10n.text("hub.HubDashboardView.12.1336", fallback: "Starting Hub…")
            case .checkingSourceStopped: return HubL10n.text("hub.sourceTakeover.checkingStopped", fallback: "Checking that the source application is stopped…")
            case .takingOver: return HubL10n.text("hub.sourceTakeover.takingOver", fallback: "Taking over the Tesla login…")
            case .fleet: return HubL10n.text("hub.fleetSetup.busy", fallback: "Working on the Fleet set-up…")
            }
        }
    }
    private var errorMessage: String?
    /// The service is set up and running but no collection window is open: the sign-in page
    /// shows the idle notice, not an error, and its only action is Done. Cleared on leaving
    /// the page, so a later visit asks the service again.
    private var signInIdle = false
    /// The service answered the window check (any readable status): it is set up and running,
    /// so closing this window is allowed whichever page is showing. Set by the check, never by
    /// the page: Back does not clear it; a service that stops answering does.
    private var serviceConfirmedRunning = false
    /// Which refresh of the window state the page still waits for, so a late answer for an
    /// earlier page visit cannot overwrite a newer one. Leaving the page moves it on.
    private var windowObservation: UInt64 = 0
    private var windowObservationInFlight = false
    /// A page visit or a return to the window asked while a check was still outstanding: that
    /// answer may predate the change, so it is discarded and one fresh check runs instead.
    private var windowObservationPending = false
    private var errorFromWindowCheck = false
    private var lastRenderedRoute: HubOnboardingRoute?
    private var migrationDiagnostic: SourceSSHDiagnostic?
    private var compatibility: HubSourceCheckResult?
    private var checks: [HubOnboardingCheck] = []
    private var verificationFinished = false
    private var handoverAcknowledged = false
    private var authWindow: TeslaAuthWindowController?
    private var logsWindow: LogsWindowController?
    private weak var logsReturnResponder: NSView?

    private let continueButton = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.168.1852", fallback: "Continue"), target: nil, action: nil)
    private let backButton = HubActionButton(title: HubL10n.text("hub.HubUtilityWindow.24.1603", fallback: "Back"), target: nil, action: nil)
    private let cancelButton = HubActionButton(title: HubL10n.text("hub.ImportSheetController.112.1706", fallback: "Cancel"), target: nil, action: nil)
    private let spinner = NSProgressIndicator()
    private let migrationSpinner = NSProgressIndicator()
    private let migrationProgress = NSProgressIndicator()
    private let migrationProgressCaption = NSTextField(labelWithString: "")
    private var migrationProgressPhase: String?
    private let footerSpinner = NSProgressIndicator()
    private var continueWidthConstraint: NSLayoutConstraint?
    private var continueHeightConstraint: NSLayoutConstraint?
    private var backWidthConstraint: NSLayoutConstraint?
    private var footerLogsButton: NSButton?

    // Fleet route (M09-T12). The client secret lives only in its secure field: it goes to the
    // command line on standard input, is cleared as each call starts and whenever its step ends,
    // and is never written to disk, a log, a preference or the pasteboard.
    private let fleetClientID = NSTextField(string: "")
    private let fleetClientSecret = NSSecureTextField(string: "")
    private let fleetRedirectURI = NSTextField(string: "")
    private let fleetDomain = NSTextField(string: "")
    private let fleetRegion = HubFormPopUpButton(frame: .zero)
    private let fleetChecklist: [NSButton] = (0..<3).map { _ in NSButton(checkboxWithTitle: "", target: nil, action: nil) }
    private let fleetConsent = NSButton(checkboxWithTitle: "", target: nil, action: nil)
    private var fleetStep: HubFleetStep = .prerequisites
    /// The waiting state's line (what the Hub waits for and the next step), from `preflight`.
    private var fleetNotice: String?
    /// `preflight` named a set-up state this app does not know: shown, and no next step offered.
    private var fleetStateUnknown: String?
    /// The Fleet preparation failed (R1): its text is shown and Continue offers to try it again.
    private var fleetPreparationFailed: String?
    /// The figures the consent page rendered (R3): acceptance records exactly these, and only
    /// while they are still the figures in effect.
    private var fleetConsentShown: HubFleetConsentFigures?
    /// The service's answer to `setup-fleet figures` the consent page shows (R2); nil until it
    /// is read on this visit of the page.
    private var fleetConsentAnswer: HubFleetConsentReading?
    private var fleetFiguresReading = false
    /// Where an accepted consent page continues: the sign-in on a first set-up, or the step a
    /// waiting state names on a return.
    private var fleetResumeAfterConsent: HubFleetStep = .signIn
    private var fleetPublicKey: HubFleetPublicKey?
    private var fleetStateReading: UInt64 = 0
    private var lastRenderedFleetStep: HubFleetStep?
    /// The client secret of the sign-in attempt waiting for the Tesla window (between `begin` and
    /// the redeem), held only in the attempt's wiping buffer, never in a control. Its identity is
    /// the attempt's: a callback for any other attempt is ignored. `endFleetSignIn` wipes it.
    private var fleetSignInSecret: HubFleetSecret?
    private var quitObservation: NSObjectProtocol?
    #if DEBUG
    /// Every secret buffer the Fleet route made, so tests can check each was wiped.
    private(set) var fleetSecretsForTests: [HubFleetSecret] = []
    #endif
    /// Test seam: opens the Fleet sign-in's browser leg; throwing stands for a window that could
    /// not be built. Nil opens `TeslaAuthWindowController`.
    var presentFleetSignIn: ((FleetOAuthFlow, @escaping (Result<TeslaAuthCodeGrant, Error>) -> Void) throws -> Void)?
    /// Test seam: asks where to save the public key.
    var presentFleetKeySavePanel: (NSSavePanel, NSWindow, @escaping (URL?) -> Void) -> Void = { panel, window, completion in
        panel.beginSheetModal(for: window) { response in completion(response == .OK ? panel.url : nil) }
    }

    private let migrationServer = NSTextField(string: "")
    private let migrationUser = NSTextField(string: "user")
    private let migrationPort = NSTextField(string: "22")
    private let migrationAuthentication = HubFormPopUpButton(frame: .zero)
    private let migrationIdentityFile = NSTextField(string: "")
    private let migrationSSHPassword = NSSecureTextField(string: "")
    private let migrationUseSudo = NSButton(
        checkboxWithTitle: HubL10n.text("hub.OnboardingWindowController.195.1856", fallback: "Use passwordless sudo for Docker access"),
        target: nil,
        action: nil
    )
    private let migrationUseLocalHistory = NSButton(
        checkboxWithTitle: HubL10n.text("hub.OnboardingWindowController.200.1857", fallback: "Use a local PostgreSQL snapshot (history only)"), target: nil, action: nil
    )
    private let migrationVersionAcknowledgement = NSButton(
        checkboxWithTitle: HubL10n.text("hub.import.ackRelease", fallback: "I confirm that the source application is at release 4.4.0 or later and will not be updated during the import."),
        target: nil,
        action: nil
    )
    private var migrationConnectButton: NSButton?
    private var migrationKeyViews: [NSView] = []
    private var pageKeyViews: [NSView] = []
    private let migrationSource = NSTextField(string: "")
    private let migrationCarID = NSTextField(string: "")
    private let migrationPasswordFile = NSTextField(string: "")
    private var migrationSession: SourceServerImportSession?
    private var migrationPreparation: HubMigrationPreparation?
    /// The source application's encryption key (M09-T05): typed once, passed on standard
    /// input to the takeover and cleared as that call starts. Never stored or logged.
    private let sourceEncryptionKey = NSSecureTextField(string: "")
    /// What the last stop check saw for the connected session; nil before one finished.
    private var sourceRunState: SourceRunState?
    private var sourceStopCheckError: String?
    /// The import finished and its session is kept for the takeover (and its Retry).
    private var importedAwaitingTakeover = false
    /// The takeover after a finished import failed: Retry, or Sign in with Tesla instead.
    private var takeoverFailure: HubSourceTakeoverRefusal?
    /// The owner chose Sign in with Tesla instead; resume the ordinary sign-in page.
    private var takeoverDeclined = false
    private var accountSignIn: HubAccountSignIn?
    /// Hears the controller drop the held discovery retry at its deadline, to update the page.
    private var discoveryRetryExpiryObserver: NSObjectProtocol?
    /// How many cars the owner sign-in configured, shown on the verification page.
    private var configuredCarsNote: String?
    private var lifecycleObservation: HubLifecycleAuthority.Observation?
    private var connectedMigrationIdentity: String?
    private var onboardingContainer: HubOnboardingContainerView!
    private var onboardingToolbar: HubOnboardingToolbar?
    private var renderedStep: Int?
    private var diagnosticView: NSView?
    private var filePanelOpen = false
    private let presentFilePanel: (NSOpenPanel, NSWindow, @escaping (URL?) -> Void) -> Void

    init(controller: HubController,
         resumeMigrationHandoverPhase: HubMigrationHandoverPhase? = nil,
         initialRoute: HubOnboardingRoute? = nil,
         previewRoute: String? = nil,
         dismissalPolicy: HubOnboardingDismissalPolicy = .accountManagement,
         closesWindowOnCancel: Bool = true,
         onDismiss: @escaping () -> Void = {},
         presentFilePanel: @escaping (NSOpenPanel, NSWindow, @escaping (URL?) -> Void) -> Void = { panel, window, completion in
             panel.beginSheetModal(for: window) { response in
                 completion(response == .OK ? panel.url : nil)
             }
         },
         onComplete: @escaping (HubOnboardingCompletion) -> Void) {
        let effectivePreviewRoute = controller.previewMode ? previewRoute : nil
        self.controller = controller
        self.previewRoute = effectivePreviewRoute
        self.dismissalPolicy = dismissalPolicy
        self.closesWindowOnCancel = closesWindowOnCancel
        self.onDismiss = onDismiss
        self.onComplete = onComplete
        self.presentFilePanel = presentFilePanel
        let shouldAutoVerify: Bool
        let resumeMessage: String?
        if let resumeMigrationHandoverPhase {
            let route: HubOnboardingRoute
            switch resumeMigrationHandoverPhase {
            case .importing:
                route = .migration
                shouldAutoVerify = false
                resumeMessage = HubL10n.text("hub.OnboardingWindowController.252.1859", fallback: "The previous import did not finish. Check the source application and run the import again.")
            case .awaitingTakeover:
                route = .migration
                shouldAutoVerify = false
                resumeMessage = nil
                importedAwaitingTakeover = true
            case .awaitingSignIn:
                route = .legacy
                shouldAutoVerify = false
                resumeMessage = nil
                takeoverDeclined = true
            case .awaitingVerification:
                route = .verify
                shouldAutoVerify = true
                resumeMessage = nil
            case .awaitingHandover:
                route = .finish
                shouldAutoVerify = false
                resumeMessage = nil
            }
            state = HubOnboardingState(
                route: route,
                path: .migration,
                provider: .legacy
            )
        } else {
            state = HubFleetOnboarding.offered(Self.previewState(route: effectivePreviewRoute)
                ?? initialRoute.map { Self.initialState(route: $0) }
                ?? HubOnboardingState())
            shouldAutoVerify = false
            resumeMessage = nil
        }
        let window = HubOnboardingSheetStyle.makeWindow(
            contentSize: HubMetrics.windowSize,
            dismissible: dismissalPolicy == .accountManagement
        )
        window.center()
        super.init(window: window)
        if resumeMigrationHandoverPhase != nil && controller.pendingMigrationHandoverIsHistoryOnly {
            migrationUseLocalHistory.state = .on
        }
        let onboardingToolbar = HubOnboardingToolbar()
        self.onboardingToolbar = onboardingToolbar
        window.toolbar = onboardingToolbar.toolbar
        window.toolbarStyle = .unified
        window.titlebarSeparatorStyle = .automatic
        if effectivePreviewRoute == "migration-connected" {
            migrationServer.stringValue = "server.local"
            compatibility = HubSourceCheckResult(
                compatible: true,
                message: HubL10n.text("hub.OnboardingWindowController.292.1862", fallback: "Ready to import."),
                reasonCode: "preview",
                requiredVersion: HubSourceCheckResult.requiredSourceRelease, schemaRecognised: true, schemaSetLabel: "0123456789ab"
            )
        } else if effectivePreviewRoute == "migration-error" {
            migrationDiagnostic = SourceSSHDiagnostic(
                reasonCode: "preview", title: HubL10n.text("hub.OnboardingWindowController.298.1866", fallback: "Source application connection failed"),
                summary: HubL10n.text("hub.OnboardingWindowController.299.1867", fallback: "This account cannot access Docker."),
                suggestions: [HubL10n.text("hub.OnboardingWindowController.300.1868", fallback: "Check the server account and Docker access, then try again.")],
                recoveryActions: [.openLogs])
        } else if effectivePreviewRoute == "importing" {
            busy = true
            busyOperation = .importing
            busyMessage = BusyOperation.importing.message
            migrationProgress.isIndeterminate = false
            migrationProgress.minValue = 0
            migrationProgress.maxValue = 1
            migrationProgress.doubleValue = 0.62
        } else if effectivePreviewRoute == "verify-loading" {
            busy = true
        } else if effectivePreviewRoute == "verify" {
            checks = HubController.previewOnboardingChecks
            verificationFinished = true
        }
        window.delegate = self
        errorMessage = resumeMessage
        configureContainer(in: window)
        configureFields()
        render()
        updateWindowCloseAvailability()
        if let resumeMigrationHandoverPhase, resumeMigrationHandoverPhase != .importing,
           !controller.previewMode && !controller.pendingMigrationHandoverIsHistoryOnly {
            setBusy(true, operation: .takingOver)
            controller.reconcileSourceTakeover { [weak self] result in
                guard let self else { return }
                self.setBusy(false)
                if case let .failure(error) = result { self.errorMessage = error.localizedDescription }
                if (self.importedAwaitingTakeover || self.takeoverDeclined) && controller.sourceTakeoverStatus.hasLogin {
                    do {
                        if self.takeoverDeclined && controller.sourceTakeoverStatus == .otherLogin {
                            try controller.completeImportedHistorySignIn()
                            self.state.path = .newInstallation
                        } else { try controller.advanceTakeoverPhase(to: .awaitingVerification) }
                        self.takeoverDeclined = false
                        self.closeMigrationSession()
                        self.importedAwaitingTakeover = false
                        self.state.route = .verify
                        self.render()
                        self.runVerification()
                    } catch { self.showInlineError(error.localizedDescription) }
                } else {
                    self.render()
                    if shouldAutoVerify { self.runVerification() }
                }
            }
        } else if shouldAutoVerify {
            DispatchQueue.main.async { [weak self] in self?.runVerification() }
        }
        lifecycleObservation = controller.observeLifecycle { [weak self] _ in self?.updateFooter() }
        // Quitting ends a sign-in attempt and wipes its secret.
        quitObservation = NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification, object: nil, queue: .main
        ) { [weak self] _ in self?.fleetApplicationWillTerminate() }
    }

    private static func previewState(route: String?) -> HubOnboardingState? {
        switch route {
        case "welcome": return HubOnboardingState(route: .welcome)
        case "choose": return HubOnboardingState(route: .choose)
        case "choose-migration": return HubOnboardingState(route: .choose, path: .migration)
        case "provider": return HubOnboardingState(route: .provider)
        case "fleet": return HubOnboardingState(route: .fleet, provider: .fleet)
        case "legacy": return HubOnboardingState(route: .legacy, provider: .legacy)
        case "migration", "migration-error": return HubOnboardingState(route: .migration, path: .migration)
        case "migration-connected": return HubOnboardingState(route: .migration, path: .migration)
        case "importing": return HubOnboardingState(route: .migration, path: .migration)
        case "verify", "verify-loading": return HubOnboardingState(route: .verify)
        case "finish": return HubOnboardingState(route: .finish)
        case "finish-migration": return HubOnboardingState(route: .finish, path: .migration)
        default: return nil
        }
    }

    private static func initialState(route: HubOnboardingRoute) -> HubOnboardingState {
        switch route {
        case .migration:
            return HubOnboardingState(route: .migration, path: .migration, provider: .legacy)
        case .legacy:
            return HubOnboardingState(route: .legacy, path: .newInstallation, provider: .legacy)
        case .fleet:
            return HubOnboardingState(route: .fleet, path: .newInstallation, provider: .fleet)
        default:
            return HubOnboardingState(route: route)
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    var currentRoute: HubOnboardingRoute { state.route }
    var operationPreventsQuit: Bool { interactionBlocked }
    var canCancel: Bool {
        (dismissalPolicy == .accountManagement || serviceConfirmedRunning) && !closeBlocked
    }
    var signInIsIdle: Bool { signInIdle }
    var serviceIsConfirmedRunning: Bool { serviceConfirmedRunning }

    func windowDidBecomeKey(_ notification: Notification) {
        // The owner came back to the window: the collection window may have opened or closed.
        refreshCollectionWindowState()
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard canCancel else {
            HubUIPresentation.playRefusalSound()
            return false
        }
        return true
    }

    /// The app is quitting: a Fleet sign-in attempt ends and its secret is wiped. (A secret
    /// buffer released with this window wipes itself as well.)
    func fleetApplicationWillTerminate() {
        endFleetSignIn()
    }

    func windowWillClose(_ notification: Notification) {
        sourceEncryptionKey.stringValue = ""
        endFleetSignIn()
        fleetStateReading &+= 1
        resetMigrationProgress()
        closeMigrationSession()
        discardDiscoveryRetry()
        guard !didNotifyDismiss else { return }
        didNotifyDismiss = true
        onDismiss()
    }

    /// The prepared session and any preparation still running belong to the controller:
    /// a window going away cancels its interest and the controller closes what remains.
    private func closeMigrationSession() {
        sourceEncryptionKey.stringValue = ""
        migrationSSHPassword.stringValue = ""
        sourceRunState = nil
        sourceStopCheckError = nil
        migrationPreparation?.cancel()
        migrationPreparation = nil
        if let session = migrationSession { controller.closeMigrationSession(session) }
        migrationSession = nil
    }

    static func routeChangeAllowed(busy: Bool,
                                   authenticationActive: Bool,
                                   migrationHandoverPending: Bool) -> Bool {
        !busy && !authenticationActive && !migrationHandoverPending
    }

    func navigate(to route: HubOnboardingRoute) {
        guard Self.routeChangeAllowed(
            busy: busy,
            authenticationActive: authWindow != nil,
            migrationHandoverPending: controller.hasPendingMigrationHandover
        ) else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        state = HubFleetOnboarding.offered(Self.initialState(route: route))
        errorMessage = nil
        errorFromWindowCheck = false
        signInIdle = false
        compatibility = nil
        migrationVersionAcknowledgement.state = .off
        closeMigrationSession()
        connectedMigrationIdentity = nil
        checks = []
        verificationFinished = false
        handoverAcknowledged = false
        importedAwaitingTakeover = false
        takeoverFailure = nil
        endFleetSignIn()
        fleetStep = .prerequisites
        fleetResumeAfterConsent = .signIn
        fleetNotice = nil
        fleetStateUnknown = nil
        fleetPreparationFailed = nil
        fleetConsentShown = nil
        fleetConsentAnswer = nil
        fleetPublicKey = nil
        sourceEncryptionKey.stringValue = ""
        render()
    }

    private func configureContainer(in window: NSWindow) {
        let header = NSView()
        header.identifier = NSUserInterfaceItemIdentifier("onboarding.header")

        let footer = HubOnboardingChromeView()
        footer.identifier = NSUserInterfaceItemIdentifier("onboarding.footer")
        let footerLine = NSBox()
        footerLine.boxType = .separator
        footerLine.translatesAutoresizingMaskIntoConstraints = false
        footer.addSubview(footerLine)

        NSLayoutConstraint.activate([
            footerLine.leadingAnchor.constraint(equalTo: footer.leadingAnchor),
            footerLine.trailingAnchor.constraint(equalTo: footer.trailingAnchor),
            footerLine.topAnchor.constraint(equalTo: footer.topAnchor),
        ])

        onboardingContainer = HubOnboardingContainerView(
            headerView: header,
            headerHeight: 0,
            footerView: footer
        )
        window.contentView = onboardingContainer
    }

    private func configureFields() {
        configureFormPopup(fleetRegion, identifier: "onboarding.fleet-region")
        configureFormPopup(migrationAuthentication, identifier: "onboarding.migration-authentication")
        fleetRegion.addItems(withTitles: [
            HubL10n.text("hub.OnboardingWindowController.437.1892", fallback: "Europe, Middle East and Africa"),
            HubL10n.text("hub.OnboardingWindowController.438.1893", fallback: "North America and Asia Pacific"),
            HubL10n.text("hub.OnboardingWindowController.china_region", fallback: "China")
        ])
        migrationAuthentication.addItems(withTitles: [HubL10n.text("hub.OnboardingWindowController.441.1895", fallback: "SSH key"),
                                                 HubL10n.text("hub.OnboardingWindowController.password_label", fallback: "Password")])
        migrationAuthentication.target = self
        migrationAuthentication.action = #selector(migrationAuthenticationChanged)
        migrationUseSudo.state = .off
        migrationUseSudo.title = ""
        migrationUseSudo.setAccessibilityLabel("This user needs sudo to read the source database")
        migrationUseSudo.controlSize = .regular
        migrationUseLocalHistory.target = self
        migrationUseLocalHistory.action = #selector(localHistorySelectionChanged)
        migrationUseLocalHistory.identifier = NSUserInterfaceItemIdentifier("onboarding.local-history")
        migrationVersionAcknowledgement.title = ""
        migrationVersionAcknowledgement.setAccessibilityLabel(
            HubL10n.text("hub.import.ackRelease", fallback: "I confirm that the source application is at release 4.4.0 or later and will not be updated during the import.")
        )
        migrationVersionAcknowledgement.controlSize = .regular
        migrationVersionAcknowledgement.target = self
        migrationVersionAcknowledgement.action = #selector(migrationVersionAcknowledgementChanged)
        migrationServer.delegate = self
        sourceEncryptionKey.delegate = self
        sourceEncryptionKey.identifier = NSUserInterfaceItemIdentifier("onboarding.source-encryption-key")
        sourceEncryptionKey.placeholderString = HubL10n.text("hub.sourceTakeover.keyPlaceholder", fallback: "Paste the encryption key")
        sourceEncryptionKey.setAccessibilityLabel(HubL10n.text("hub.sourceTakeover.keyLabel", fallback: "Encryption key"))
        migrationUser.delegate = self
        migrationPort.delegate = self
        migrationSSHPassword.delegate = self
        migrationServer.identifier = NSUserInterfaceItemIdentifier("onboarding.source-server")
        migrationUser.identifier = NSUserInterfaceItemIdentifier("onboarding.source-user")
        migrationPort.identifier = NSUserInterfaceItemIdentifier("onboarding.source-port")
        migrationSSHPassword.identifier = NSUserInterfaceItemIdentifier("onboarding.source-password")
        for field in [fleetClientID, fleetClientSecret, fleetRedirectURI, fleetDomain] {
            field.delegate = self
        }
        fleetClientID.identifier = NSUserInterfaceItemIdentifier("onboarding.fleet.client-id")
        fleetClientSecret.identifier = NSUserInterfaceItemIdentifier("onboarding.fleet.client-secret")
        fleetRedirectURI.identifier = NSUserInterfaceItemIdentifier("onboarding.fleet.redirect-uri")
        fleetDomain.identifier = NSUserInterfaceItemIdentifier("onboarding.fleet.domain")
        fleetClientSecret.placeholderString = HubL10n.text("hub.fleetSetup.secretPlaceholder", fallback: "Client secret")
        fleetClientSecret.setAccessibilityLabel(HubL10n.text("hub.fleetSetup.secretLabel", fallback: "Client secret"))
        fleetRedirectURI.placeholderString = "https://example.com/tesla/callback"
        fleetDomain.placeholderString = "example.com"
        for (index, box) in fleetChecklist.enumerated() {
            box.identifier = NSUserInterfaceItemIdentifier("onboarding.fleet.prerequisite.\(index)")
            box.target = self
            box.action = #selector(fleetChecklistChanged)
            box.controlSize = .regular
        }
        fleetConsent.identifier = NSUserInterfaceItemIdentifier("onboarding.fleet.consent-accept")
        fleetConsent.target = self
        fleetConsent.action = #selector(fleetChecklistChanged)
        fleetConsent.controlSize = .regular
        fleetConsent.setAccessibilityLabel(Self.fleetConsentText)
        for field in [fleetClientID, fleetClientSecret, fleetRedirectURI, fleetDomain,
                      migrationServer, migrationUser, migrationPort,
                      migrationIdentityFile, migrationSSHPassword, sourceEncryptionKey,
                      migrationSource, migrationCarID, migrationPasswordFile] {
            field.controlSize = .regular
            field.font = HubTypography.body
            field.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
        }
        migrationSource.placeholderString = HubL10n.text("hub.OnboardingWindowController.475.1901", fallback: "Password-free local PostgreSQL URL")
        migrationCarID.placeholderString = HubL10n.text("hub.ImportSheetController.65.1695", fallback: "Car ID")
        migrationPasswordFile.placeholderString = HubL10n.text("hub.OnboardingWindowController.477.1903", fallback: "Owner-only PostgreSQL password file")
        migrationSource.delegate = self
        migrationCarID.delegate = self
        migrationPasswordFile.delegate = self
        fleetClientID.placeholderString = HubL10n.text("hub.OnboardingWindowController.482.1905", fallback: "Tesla application client ID")
        migrationServer.placeholderString = "server.local"
        migrationIdentityFile.placeholderString = HubL10n.text("hub.OnboardingWindowController.486.1909", fallback: "Optional — uses SSH agent or default keys")
        migrationSSHPassword.placeholderString = HubL10n.text("hub.OnboardingWindowController.487.1910", fallback: "SSH password")
    }

    private func configureFormPopup(_ popup: NSPopUpButton, identifier: String) {
        popup.identifier = NSUserInterfaceItemIdentifier(identifier)
        // The stock bezel is shorter than its constrained frame. These remain
        // native pop-up controls; HubFormPopUpButton only paints the same
        // full-height form surface used by the adjacent text fields.
        popup.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
    }

    private func render() {
        guard let window else { return }
        state = HubFleetOnboarding.offered(state)
        let previousStep = renderedStep
        renderedStep = state.step
        diagnosticView = nil
        let operation = focusedOperation
        if previousStep == nil || !window.isVisible {
            window.setContentSize(currentSheetSize)
        }
        let previousField: NSView? = {
            if let editor = window.firstResponder as? NSTextView,
               let delegate = editor.delegate as? NSView { return delegate }
            return window.firstResponder as? NSView
        }()
        window.initialFirstResponder = nil
        window.makeFirstResponder(nil)
        migrationKeyViews = []
        pageKeyViews = []

        let content: NSView
        if let operation {
            content = operationBody(operation)
        } else {
            let pageHeader = onboardingPageHeader()
            let body = pageBody()
            let page = NSStackView(views: [pageHeader, body])
            page.identifier = NSUserInterfaceItemIdentifier(
                state.route == .welcome ? "onboarding.welcome.body" : "onboarding.body"
            )
            page.orientation = .vertical
            page.alignment = .leading
            page.spacing = 24
            NSLayoutConstraint.activate([
                pageHeader.widthAnchor.constraint(equalTo: page.widthAnchor),
                body.widthAnchor.constraint(equalTo: page.widthAnchor)
            ])
            content = page
        }
        content.translatesAutoresizingMaskIntoConstraints = false

        let footerContent = footerView()
        onboardingContainer.replaceBody(content)
        onboardingContainer.replaceFooterContent(footerContent)
        updateFooter()
        window.recalculateKeyViewLoop()
        configureKeyViewLoop(previousField: previousField)
        onboardingContainer.alphaValue = 1
        if let diagnosticView {
            onboardingContainer.reveal(diagnosticView)
        }
        if let previousStep {
            HubMotion.transition(onboardingContainer.bodyDocumentView,
                                 forward: previousStep == state.step ? nil : state.step > previousStep)
        }
        let entering = state.route == .legacy && lastRenderedRoute != .legacy
        let leaving = state.route != .legacy && lastRenderedRoute == .legacy
        let enteringFleet = state.route == .fleet && lastRenderedRoute != .fleet
        lastRenderedRoute = state.route
        // The client secret belongs to the step it was typed on: a step that ends clears it.
        let fleetPage: HubFleetStep? = state.route == .fleet ? fleetStep : nil
        if fleetPage != lastRenderedFleetStep {
            fleetClientSecret.stringValue = ""
            if fleetPage != .consent {
                // The next visit of the consent page asks the service for its figures again.
                fleetConsent.state = .off
                fleetConsentShown = nil
                fleetConsentAnswer = nil
            }
        }
        lastRenderedFleetStep = fleetPage
        if enteringFleet {
            DispatchQueue.main.async { [weak self] in self?.refreshFleetSetupState() }
        }
        if leaving {
            // An answer still outstanding belongs to the visit that just ended.
            windowObservation &+= 1
            windowObservationPending = false
        }
        if entering { refreshCollectionWindowState() }
    }

    private var currentSheetSize: NSSize { HubMetrics.windowSize }

    private var focusedOperation: HubOnboardingOperation? {
        if busyOperation == .importing { return .importing }
        if state.path == .newInstallation, busyOperation == .setup { return .setup }
        return nil
    }

    private func operationBody(_ operation: HubOnboardingOperation) -> NSView {
        switch operation {
        case .importing:
            return migrationProgressBody()
        case .setup:
            spinner.style = .spinning
            spinner.controlSize = .regular
            spinner.startAnimation(nil)
            let title = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.558.1914", fallback: "Setting up Hub…"))
            title.font = HubTypography.heading
            title.textColor = HubPalette.foreground
            let subtitle = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.573.1916", fallback: "Saving your connection and preparing Hub."))
            subtitle.font = HubTypography.body
            subtitle.textColor = HubPalette.mutedForeground
            let stack = NSStackView(views: [title, subtitle, spinner])
            stack.orientation = .vertical
            stack.alignment = .leading
            stack.spacing = 0
            stack.setCustomSpacing(4, after: title)
            stack.setCustomSpacing(14, after: subtitle)
            return stack
        }
    }

    private var pageTitle: String {
        switch state.route {
        case .welcome: return HubL10n.text("hub.OnboardingWindowController.588.1917", fallback: "Welcome to Tesatlas Hub")
        case .choose: return HubL10n.text("hub.OnboardingWindowController.589.1918", fallback: "How would you like to start?")
        case .provider: return HubL10n.text("hub.OnboardingWindowController.590.1919", fallback: "Choose how Hub connects")
        case .fleet: return fleetPageTitle
        case .legacy: return HubL10n.text("hub.OnboardingWindowController.592.1921", fallback: "Sign in with Tesla")
        case .migration: return HubL10n.text("hub.OnboardingWindowController.593.1922", fallback: "Import History…")
        case .verify: return HubL10n.text("hub.OnboardingWindowController.594.1923", fallback: "Checking your Hub")
        case .finish: return state.path == .migration ? HubL10n.text("hub.OnboardingWindowController.595.1924", fallback: "Migration complete") : HubL10n.text("hub.OnboardingWindowController.595.1925", fallback: "Tesatlas Hub is ready")
        }
    }

    private var pageSubtitle: String {
        switch state.route {
        case .welcome:
            return HubL10n.text("hub.OnboardingWindowController.602.1926", fallback: "Your own Tesla telemetry collector, running privately on this Mac.")
        case .choose:
            return HubL10n.text("hub.OnboardingWindowController.604.1927", fallback: "Set up a fresh Hub or bring your history over from the source application.")
        case .provider:
            return HubL10n.text("hub.OnboardingWindowController.606.1928", fallback: "Sign in with your Tesla owner account to start. Fleet Telemetry is available if you prefer it.")
        case .fleet:
            return fleetPageSubtitle
        case .legacy:
            return HubL10n.text("hub.signIn.pageSubtitle", fallback: "Sign in with Tesla while a collection window is open.")
        case .migration:
            return HubL10n.text("hub.OnboardingWindowController.612.1931", fallback: "Connect to your source application server to import its vehicle history.")
        case .verify:
            return HubL10n.text("hub.OnboardingWindowController.614.1932", fallback: "Making sure everything is wired up correctly.")
        case .finish:
            return state.path == .migration
                ? HubL10n.text("hub.OnboardingWindowController.617.1933", fallback: "Your vehicle history has been imported into Hub.")
                : HubL10n.text("hub.OnboardingWindowController.618.1934", fallback: "Hub is set up and ready to start collecting vehicle data.")
        }
    }

    private var pageSymbol: String {
        switch state.route {
        case .welcome: return "checkmark.shield"
        case .choose: return "arrow.triangle.branch"
        case .provider, .fleet: return "antenna.radiowaves.left.and.right"
        case .legacy: return "key"
        case .migration: return "square.and.arrow.down"
        case .verify: return "stethoscope"
        case .finish: return "checkmark.circle"
        }
    }

    private func onboardingPageHeader() -> NSView {
        let tile = HubIconTileView(symbol: pageSymbol, accessibilityDescription: pageTitle,
                                   size: 48, symbolSize: 24, weight: .medium,
                                   fill: .elevated,
                                   tint: [.welcome, .finish].contains(state.route)
                                       ? HubPalette.success : .labelColor,
                                   radius: 12)
        let title = NSTextField(labelWithString: pageTitle)
        title.font = HubTypography.heading
        title.textColor = HubPalette.foreground
        title.alignment = .natural
        let subtitle = NSTextField(wrappingLabelWithString: pageSubtitle)
        subtitle.font = .systemFont(ofSize: 14)
        subtitle.textColor = HubPalette.mutedForeground
        subtitle.alignment = .natural
        subtitle.maximumNumberOfLines = 2
        let copy = NSStackView(views: [title, subtitle])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 2
        if [.welcome, .choose, .provider, .legacy, .finish].contains(state.route) {
            title.alignment = .center
            subtitle.alignment = .center
            copy.alignment = .centerX
            let column = NSStackView(views: [tile, copy])
            column.orientation = .vertical
            column.alignment = .centerX
            column.spacing = 12
            copy.widthAnchor.constraint(equalTo: column.widthAnchor).isActive = true
            return column
        }
        let row = NSStackView(views: [tile, copy])
        row.spacing = 16
        row.alignment = .centerY
        return row
    }

    private func pageBody() -> NSView {
        switch state.route {
        case .welcome: return welcomeBody()
        case .choose: return chooseBody()
        case .provider: return providerBody()
        case .fleet: return fleetBody()
        case .legacy: return legacyBody()
        case .migration: return migrationBody()
        case .verify: return verifyBody()
        case .finish: return finishBody()
        }
    }

    private func welcomeBody() -> NSView {
        let card = HubCardView()
        let rows = [
            onboardingFeatureRow(symbol: "car.side", title: HubL10n.text("hub.OnboardingWindowController.687.1943", fallback: "Connect your Tesla"),
                                 subtitle: HubL10n.text("hub.OnboardingWindowController.688.1944", fallback: "Collect vehicle data in the background.")),
            onboardingFeatureRow(symbol: "cylinder", title: HubL10n.text("hub.OnboardingWindowController.689.1946", fallback: "Keep your history here"),
                                 subtitle: HubL10n.text("hub.OnboardingWindowController.690.1947", fallback: "Store data locally on this Mac.")),
            onboardingFeatureRow(symbol: "square.and.arrow.down", title: HubL10n.text("hub.OnboardingWindowController.691.1949", fallback: "Bring your existing history"),
                                 subtitle: HubL10n.text("hub.OnboardingWindowController.692.1950", fallback: "Import from the source application when you are ready."))
        ]
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.spacing = 0
        for (index, row) in rows.enumerated() {
            if index > 0 { stack.addArrangedSubview(separator()) }
            stack.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        stack.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            stack.topAnchor.constraint(equalTo: card.topAnchor),
            stack.bottomAnchor.constraint(equalTo: card.bottomAnchor)
        ])
        return card
    }

    private func onboardingFeatureRow(symbol: String, title: String, subtitle: String) -> NSView {
        let tile = HubIconTileView(symbol: symbol, accessibilityDescription: title)
        tile.imageView.identifier = NSUserInterfaceItemIdentifier("onboarding.welcome.feature-icon")
        let label = NSTextField(labelWithString: title)
        label.font = .systemFont(ofSize: 14, weight: .medium)
        label.textColor = HubPalette.foreground
        let detail = NSTextField(labelWithString: subtitle)
        detail.font = .systemFont(ofSize: 12)
        detail.textColor = .secondaryLabelColor
        let copy = NSStackView(views: [label, detail])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 1
        let row = NSStackView(views: [tile, copy, spacer()])
        row.edgeInsets = NSEdgeInsets(top: 10, left: HubMetrics.rowHorizontalInset,
                                     bottom: 10, right: HubMetrics.rowHorizontalInset)
        row.spacing = 12
        row.alignment = .centerY
        row.heightAnchor.constraint(equalToConstant: HubMetrics.rowHeight).isActive = true
        return row
    }

    private func chooseBody() -> NSView {
        let fresh = choiceButton(title: HubL10n.text("hub.OnboardingWindowController.736.1952", fallback: "New installation"),
                                 subtitle: HubL10n.text("hub.OnboardingWindowController.737.1953", fallback: "Start with a fresh database."),
                                 selected: state.path == .newInstallation,
                                 action: #selector(selectNewInstallation))
        let migration = choiceButton(title: HubL10n.text("hub.OnboardingWindowController.593.1922", fallback: "Import History…"),
                                     subtitle: HubL10n.text("hub.OnboardingWindowController.741.1955", fallback: "Bring your existing vehicle history."),
                                     selected: state.path == .migration,
                                     action: #selector(selectMigration))
        return verticalChoices([fresh, migration])
    }

    private func providerBody() -> NSView {
        // The owner sign-in is offered first and is the default; Fleet stays one click away.
        let legacy = choiceButton(title: HubL10n.text("hub.OnboardingWindowController.752.1958", fallback: "Owner sign-in"),
                                  subtitle: HubL10n.text("hub.OnboardingWindowController.753.1959", fallback: "Sign in with Tesla. Hub keeps its own login."),
                                  selected: state.provider == .legacy,
                                  action: #selector(selectLegacy))
        guard HubFleetOnboarding.isOffered else { return verticalChoices([legacy]) }
        let fleet = choiceButton(title: HubL10n.text("hub.fleetSetup.cardTitle", fallback: "Fleet API"),
                                 subtitle: HubL10n.text("hub.fleetSetup.cardDetail", fallback: "Polling through Tesla's Fleet API. Needs your own Tesla developer application and a domain."),
                                 selected: state.provider == .fleet,
                                 action: #selector(selectFleet))
        return verticalChoices([legacy, fleet])
    }

    private func legacyBody() -> NSView {
        // One sign-in action: the footer's primary. Sign-in is the code exchange only: no
        // token field exists.
        let privacy = featureRow(HubL10n.text("hub.signIn.privacy", fallback: "Your Tesla password goes only to Tesla. Hub keeps its own login, encrypted on this Mac."), "lock.fill")
        var views: [NSView] = []
        if signInIdle {
            views.append(signInIdleNotice())
        } else if controller.canRetryOwnerDiscovery {
            // A failed car discovery is resumed with the held sign-in, without a new Tesla page.
            let retry = HubActionButton(title: HubL10n.text("hub.signIn.discovery.retry", fallback: "Retry finding cars"),
                                        target: self, action: #selector(retryOwnerDiscovery))
            retry.identifier = NSUserInterfaceItemIdentifier("onboarding.legacy.retryDiscovery")
            configureFlatButton(retry, symbol: "arrow.clockwise")
            let buttons = NSStackView(views: [retry])
            buttons.spacing = 12
            buttons.alignment = .centerY
            views.append(buttons)
        }
        views.append(privacy)
        if serviceConfirmedRunning && !signInIdle {
            views.append(featureRow(closeKeepsRunningText, "info.circle", color: HubPalette.mutedForeground))
        }
        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        if signInIdle {
            views[0].widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        privacy.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        if let last = views.last, last !== privacy {
            last.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    /// No open window, nothing wrong: the service runs and waits for one.
    private func signInIdleNotice() -> NSView {
        noticeView(symbol: "checkmark.circle.fill", tone: .success,
                   title: HubL10n.text("hub.signIn.idle.title", fallback: "Hub is set up and running on this Mac"),
                   message: HubL10n.text("hub.signIn.idle.detail", fallback: "Tesla sign-in opens only during a collection window, and you will be told when one opens. Closing this window leaves Hub running."),
                   identifier: "onboarding.signin.idle")
    }

    private func sourceImportIntroduction() -> NSView {
        let intro = NSTextField(wrappingLabelWithString: HubL10n.text("hub.import.intro", fallback: "Import history from the PostgreSQL database of the vehicle-data software you used before (the source database). Hub checks the database first and tells you whether this release can import it. Importing only reads the source database; it never changes it."))
        let explanation = NSTextField(wrappingLabelWithString: HubL10n.text("hub.import.ackExplain", fallback: "A schema check cannot show which release of the source application wrote this database: more than one release shares this schema."))
        explanation.textColor = .secondaryLabelColor
        let stack = NSStackView(views: [intro, explanation])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        for view in stack.arrangedSubviews {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return stack
    }

    private func migrationBody() -> NSView {
        if busyOperation == .importing {
            return migrationProgressBody()
        }
        if importedAwaitingTakeover {
            return takeoverBody()
        }
        if isPreviewConnectedMigration {
            return connectedMigrationPreviewBody()
        }
        if migrationUseLocalHistory.state == .on {
            return localHistoryBody()
        }
        if let migrationSession {
            return connectedMigrationBody(migrationSession)
        }
        return migrationConnectionBody()
    }

    private func migrationConnectionBody() -> NSView {
        migrationPort.widthAnchor.constraint(equalToConstant: 72).isActive = true
        let portLabel = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.822.1978", fallback: "Port"))
        portLabel.font = HubTypography.label
        portLabel.textColor = HubPalette.mutedForeground
        let serverPort = NSStackView(views: [migrationServer, portLabel, migrationPort])
        serverPort.spacing = 10
        serverPort.alignment = .centerY
        serverPort.distribution = .fill
        migrationServer.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        var fields: [(String, NSView)] = [(HubL10n.text("hub.OnboardingWindowController.server_label", fallback: "Server"), serverPort),
                                         (HubL10n.text("hub.OnboardingWindowController.832.1980", fallback: "SSH user"), migrationUser),
                                         (HubL10n.text("hub.OnboardingWindowController.authentication_label", fallback: "Authentication"), migrationAuthentication)]
        migrationKeyViews = [migrationServer, migrationUser, migrationPort, migrationAuthentication]
        if migrationAuthentication.indexOfSelectedItem == 0 {
            let choose = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.836.1982", fallback: "Choose Key…"), target: self, action: #selector(chooseMigrationIdentity))
            choose.identifier = NSUserInterfaceItemIdentifier("onboarding.choose-ssh-key")
            choose.hubFont = HubTypography.action
            choose.setContentCompressionResistancePriority(.required, for: .horizontal)
            let keyRow = NSStackView(views: [migrationIdentityFile, choose])
            keyRow.alignment = .centerY
            keyRow.spacing = 8
            migrationIdentityFile.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
            fields.append((HubL10n.text("hub.OnboardingWindowController.441.1895", fallback: "SSH key"), keyRow))
            migrationKeyViews.append(contentsOf: [migrationIdentityFile, choose])
        } else {
            fields.append((HubL10n.text("hub.OnboardingWindowController.password_label", fallback: "Password"), migrationSSHPassword))
            migrationKeyViews.append(migrationSSHPassword)
        }
        var views: [NSView] = importedAwaitingTakeover
            ? [NSTextField(wrappingLabelWithString: HubL10n.text("hub.sourceTakeover.reconnect",
                fallback: "History is already imported. Reconnect to the source server to finish taking over the login; history will not be imported again.")), onboardingForm(fields)]
            : [sourceImportIntroduction(), migrationUseLocalHistory, onboardingForm(fields)]
        views.append(wrappingCheckbox(
            migrationUseSudo,
            title: HubL10n.text("hub.OnboardingWindowController.446.1897", fallback: "This user needs sudo to read the source database")
        ))
        migrationKeyViews.append(migrationUseSudo)
        if !importedAwaitingTakeover { migrationKeyViews.append(migrationUseLocalHistory) }

        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 7
        for view in views {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        if let compatibility {
            let status = sourceStatusRow(compatibility)
            stack.addArrangedSubview(status)
            addSourceGuidance(compatibility, to: stack)
            status.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        if let migrationDiagnostic {
            let diagnostic = migrationDiagnosticView(migrationDiagnostic)
            diagnosticView = diagnostic
            stack.addArrangedSubview(diagnostic)
            diagnostic.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    private func sourceStatusRow(_ report: HubSourceCheckResult) -> NSView {
        let row = featureRow(sourceRecognitionTitle(report),
            report.mayImport ? "checkmark.circle.fill" : "exclamationmark.triangle.fill",
            color: report.mayImport ? .systemGreen : .systemOrange)
        row.identifier = NSUserInterfaceItemIdentifier(report.recognitionKey ?? "hub.import.guidance")
        return row
    }

    #if DEBUG
    func configureLocalHistoryForTests(source: String, carID: String, passwordFile: String) {
        migrationUseLocalHistory.state = .on
        localHistorySelectionChanged()
        migrationSource.stringValue = source
        migrationCarID.stringValue = carID
        migrationPasswordFile.stringValue = passwordFile
        migrationVersionAcknowledgement.state = .on
        compatibility = nil
        render()
    }
    func showSourceConnectionFormForTests() { migrationUseLocalHistory.state = .off; render() }
    func checkLocalHistoryForTests() { checkLocalHistory() }
    func importLocalHistoryForTests() { importLocalHistory() }
    var localHistoryReportForTests: HubSourceCheckResult? { compatibility }
    func connectToSourceForTests(server: String, user: String = "user") {
        migrationServer.stringValue = server
        migrationUser.stringValue = user
        checkMigrationCompatibility()
    }
    func acknowledgeSourceReleaseForTests() {
        migrationVersionAcknowledgement.state = .on
        migrationVersionAcknowledgementChanged()
    }
    func enterSourceKeyForTests(_ key: String) {
        sourceEncryptionKey.stringValue = key
        updateFooter()
    }
    var sourceKeyFieldValueForTests: String { sourceEncryptionKey.stringValue }
    var sourceRunStateForTests: SourceRunState? { sourceRunState }
    var takeoverFailureCodeForTests: String? { takeoverFailure?.code }
    var awaitingTakeoverForTests: Bool { importedAwaitingTakeover }
    var hasMigrationSessionForTests: Bool { migrationSession != nil }
    var isBusyForTests: Bool { busy }
    var continueTitleForTests: String { continueButton.title }
    var continueEnabledForTests: Bool { continueButton.isEnabled }
    var backHiddenForTests: Bool { backButton.isHidden }
    func continueForTests() { continuePressed() }
    func checkSourceAgainForTests() { checkSourceStoppedPressed() }
    func declineTakeoverForTests() { declineTakeover() }
    func reconnectTakeoverForTests() { reconnectTakeover() }
    func abandonSourceForTests() { closeMigrationSession() }
    var takeoverDeclinedForTests: Bool { takeoverDeclined }
    func showFinishForTests() { state.route = .finish; render() }

    #endif

    private func sourceRecognitionTitle(_ report: HubSourceCheckResult) -> String { report.recognitionTitle }

    private func addSourceGuidance(_ report: HubSourceCheckResult, to stack: NSStackView) {
        guard !report.compatible, report.recognitionKey != nil, report.message != report.recognitionTitle else { return }
        let guidance = NSTextField(wrappingLabelWithString: report.message)
        guidance.maximumNumberOfLines = 0
        stack.addArrangedSubview(guidance)
        guidance.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
    }

    private func localHistoryBody() -> NSView {
        let choosePassword = HubActionButton(title: HubL10n.text("hub.ImportSheetController.110.1702", fallback: "Choose…"), target: self,
                                             action: #selector(chooseMigrationPassword))
        configureFlatButton(choosePassword)
        let passwordRow = NSStackView(views: [migrationPasswordFile, choosePassword])
        passwordRow.spacing = 8
        passwordRow.alignment = .centerY
        let note = NSTextField(wrappingLabelWithString:
            HubL10n.text("hub.OnboardingWindowController.888.1990", fallback: "History only. Hub will not inspect or import Tesla credentials. Collection remains off. The database schema can be checked; the source application version remains unknown."))
        note.maximumNumberOfLines = 0
        let stack = NSStackView(views: [sourceImportIntroduction(), migrationUseLocalHistory,
            onboardingForm([(HubL10n.text("hub.OnboardingWindowController.891.1991", fallback: "Local PostgreSQL source"), migrationSource),
                            (HubL10n.text("hub.ImportSheetController.65.1695", fallback: "Car ID"), migrationCarID),
                            (HubL10n.text("hub.ImportSheetController.69.1697", fallback: "Password file"), passwordRow)]), note,
            wrappingCheckbox(migrationVersionAcknowledgement,
                title: HubL10n.text("hub.import.ackRelease", fallback: "I confirm that the source application is at release 4.4.0 or later and will not be updated during the import."))])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        for view in stack.arrangedSubviews { view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
        if let compatibility {
            stack.addArrangedSubview(sourceStatusRow(compatibility))
            addSourceGuidance(compatibility, to: stack)
        }
        migrationKeyViews = [migrationUseLocalHistory, migrationSource, migrationCarID,
                             migrationPasswordFile, choosePassword, migrationVersionAcknowledgement]
        return withError(stack)
    }

    private func connectedMigrationBody(_ session: SourceServerImportSession) -> NSView {
        migrationConnectButton = nil
        let host = migrationServer.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        var views: [NSView] = [sourceImportIntroduction(), migrationSuccessCard(host: host)]
        if let version = session.sourceVersion {
            views.append(featureRow(HubL10n.format("hub.OnboardingWindowController.915.1997", fallback: "Source application %1$@", arguments: [String(describing: version)]), "info.circle", color: HubPalette.mutedForeground))
        }
        views.append(wrappingCheckbox(
            migrationVersionAcknowledgement,
            title: HubL10n.text("hub.import.ackRelease", fallback: "I confirm that the source application is at release 4.4.0 or later and will not be updated during the import.")
        ))
        migrationKeyViews = [migrationVersionAcknowledgement]

        if busyOperation == .checkingCompatibility {
            spinner.style = .spinning
            spinner.controlSize = .small
            spinner.startAnimation(nil)
            let checking = NSStackView(views: [spinner, NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.927.2001", fallback: "Checking…"))])
            checking.spacing = 8
            checking.alignment = .centerY
            views.append(checking)
        } else if compatibility?.mayImport == true {
            views.append(featureRow(HubL10n.text("hub.OnboardingWindowController.292.1862", fallback: "Ready to import."), "checkmark.circle", color: HubPalette.success))
        }
        if compatibility?.mayImport == true {
            views.append(sourceStopRow())
            views.append(sourceKeyField())
            migrationKeyViews.append(sourceEncryptionKey)
        }

        let change = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.935.2004", fallback: "Change Server"), target: self,
                                     action: #selector(changeMigrationServer))
        configureFlatButton(change)
        if compatibility?.mayImport == true {
            let checkAgain = HubActionButton(title: HubL10n.text("hub.sourceTakeover.checkAgain", fallback: "Check Again"),
                                             target: self, action: #selector(checkSourceStoppedPressed))
            checkAgain.identifier = NSUserInterfaceItemIdentifier("onboarding.source-check-again")
            configureFlatButton(checkAgain)
            let actions = NSStackView(views: [change, checkAgain])
            actions.spacing = 8
            views.append(actions)
            migrationKeyViews.append(contentsOf: [change, checkAgain])
        } else {
            views.append(change)
            migrationKeyViews.append(change)
        }

        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        for view in views.dropLast() {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    /// The stop gate: the source application must be stopped before the import, and Hub
    /// will take over its Tesla login afterwards.
    private func sourceStopRow() -> NSView {
        let row: NSView
        if busyOperation == .checkingSourceStopped {
            spinner.style = .spinning
            spinner.controlSize = .small
            spinner.startAnimation(nil)
            let checking = NSStackView(views: [spinner, NSTextField(labelWithString: BusyOperation.checkingSourceStopped.message)])
            checking.spacing = 8
            checking.alignment = .centerY
            row = checking
        } else if let sourceStopCheckError {
            row = featureRow(sourceStopCheckError, "exclamationmark.triangle.fill", color: .systemOrange)
        } else if sourceRunState == .stopped {
            row = featureRow(HubL10n.text("hub.sourceTakeover.stopped", fallback: "The source application is stopped. Hub will take over its Tesla login after the import."),
                             "checkmark.circle", color: HubPalette.success)
        } else if sourceRunState == .running {
            row = featureRow(HubL10n.text("hub.sourceTakeover.running", fallback: "The source application is still running. Stop it and its dashboards, then choose Check Again."),
                             "exclamationmark.triangle.fill", color: .systemOrange)
        } else {
            row = featureRow(HubL10n.text("hub.sourceTakeover.stopFirst", fallback: "Stop the source application and its dashboards. Hub will take over its Tesla login."),
                             "pause.circle", color: HubPalette.mutedForeground)
        }
        row.identifier = NSUserInterfaceItemIdentifier("onboarding.source-stop-state")
        return row
    }

    private func sourceKeyField() -> NSView {
        let field = verticalField(HubL10n.text("hub.sourceTakeover.keyLabel", fallback: "Encryption key"), sourceEncryptionKey)
        let note = NSTextField(wrappingLabelWithString: HubL10n.text(
            "hub.sourceTakeover.keyNote",
            fallback: "The source application's encryption key. Hub passes it once to the takeover and never stores it."))
        note.font = HubTypography.body
        note.textColor = HubPalette.mutedForeground
        note.maximumNumberOfLines = 0
        let stack = NSStackView(views: [field, note])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 4
        for view in stack.arrangedSubviews { view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
        return stack
    }

    /// After the import: the takeover is running, or it failed and the page offers Retry
    /// (with the key entered again) and Sign in with Tesla instead. History stays imported.
    private func takeoverBody() -> NSView {
        migrationConnectButton = nil
        migrationKeyViews = []
        var views: [NSView] = []
        if busyOperation == .takingOver {
            spinner.style = .spinning
            spinner.controlSize = .small
            spinner.startAnimation(nil)
            let running = NSStackView(views: [spinner, NSTextField(labelWithString: BusyOperation.takingOver.message)])
            running.spacing = 8
            running.alignment = .centerY
            views.append(running)
            migrationKeyViews = []
        } else {
            if let sourceStopCheckError {
                views.append(NSTextField(wrappingLabelWithString: sourceStopCheckError))
            } else if sourceRunState == .running {
                views.append(NSTextField(wrappingLabelWithString: HubL10n.text("hub.sourceTakeover.retrySourceRunning",
                    fallback: "The source application is still running. Stop it before trying the take-over again.")))
            }
            let failure = takeoverFailure ?? HubSourceTakeoverRefusal(
                code: "takeover_failed", message: HubSourceTakeoverRefusal.message(code: "takeover_failed"))
            let title = controller.sourceTakeoverStatus == .absent
                ? HubL10n.text("hub.sourceTakeover.failedTitle", fallback: "History imported; Tesla login not taken over")
                : HubL10n.text("hub.sourceTakeover.pendingTitle", fallback: "History imported; confirm the Tesla login take-over")
            views.append(noticeView(symbol: "exclamationmark.triangle.fill", tone: .warning,
                                    title: title,
                                    message: failure.message,
                                    identifier: "onboarding.takeover-failure"))
            if migrationSession == nil {
                views.append(migrationConnectionBody())
            } else {
                views.append(sourceKeyField())
                let reconnect = HubActionButton(title: HubL10n.text("hub.sourceTakeover.reconnectButton", fallback: "Reconnect to Server"),
                    target: self, action: #selector(reconnectTakeover))
                reconnect.identifier = NSUserInterfaceItemIdentifier("onboarding.takeover-reconnect")
                configureFlatButton(reconnect)
                views.append(reconnect)
                migrationKeyViews.append(reconnect)
            }
            let signIn = HubActionButton(title: HubL10n.text("hub.sourceTakeover.signInInstead", fallback: "Sign in with Tesla Instead"),
                                         target: self, action: #selector(declineTakeover))
            signIn.identifier = NSUserInterfaceItemIdentifier("onboarding.takeover-sign-in")
            configureFlatButton(signIn)
            views.append(signIn)
            migrationKeyViews.append(signIn)
            if migrationSession != nil { migrationKeyViews.append(sourceEncryptionKey) }
        }
        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        for view in views where !(view is NSButton) {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    private var isPreviewConnectedMigration: Bool {
        controller.previewMode && previewRoute == "migration-connected"
    }

    private func connectedMigrationPreviewBody() -> NSView {
        migrationConnectButton = nil
        migrationKeyViews = [migrationVersionAcknowledgement]
        let stack = NSStackView(views: [
            migrationSuccessCard(host: migrationServer.stringValue),
            wrappingCheckbox(
                migrationVersionAcknowledgement,
                title: HubL10n.text("hub.import.ackRelease", fallback: "I confirm that the source application is at release 4.4.0 or later and will not be updated during the import.")
            )
        ])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 16
        for view in stack.arrangedSubviews {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return stack
    }

    private func migrationSuccessCard(host: String) -> NSView {
        let icon = NSImageView(image: symbolImage("checkmark.circle", description: nil))
        icon.setAccessibilityElement(false)
        icon.image = icon.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 16, weight: .medium)
        )
        icon.contentTintColor = HubPalette.success
        icon.widthAnchor.constraint(equalToConstant: 16).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 16).isActive = true
        let title = NSTextField(labelWithString: HubL10n.format("hub.OnboardingWindowController.983.2008", fallback: "Connected to %1$@", arguments: [String(describing: host)]))
        title.font = HubTypography.emphasis
        let detail = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.985.2009", fallback: "Found a source database ready to import."))
        detail.font = HubTypography.body
        detail.textColor = HubPalette.mutedForeground
        let copy = NSStackView(views: [title, detail])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 2
        let row = NSStackView(views: [icon, copy])
        row.spacing = 11
        row.alignment = .centerY
        row.translatesAutoresizingMaskIntoConstraints = false
        let card = HubCardView()
        card.addSubview(row)
        NSLayoutConstraint.activate([
            card.heightAnchor.constraint(equalToConstant: 58),
            row.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            row.trailingAnchor.constraint(lessThanOrEqualTo: card.trailingAnchor, constant: -14),
            row.centerYAnchor.constraint(equalTo: card.centerYAnchor)
        ])
        return card
    }

    private func migrationProgressBody() -> NSView {
        migrationProgress.identifier = NSUserInterfaceItemIdentifier("onboarding.migration-progress")
        migrationProgress.style = .bar
        migrationProgress.isIndeterminate = false
        migrationProgress.controlSize = .regular

        let title = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.304.1870", fallback: "Importing data…"))
        title.font = HubTypography.heading
        title.textColor = HubPalette.foreground
        let subtitle = migrationProgressCaption
        subtitle.stringValue = migrationProgressCaptionText
        subtitle.font = HubTypography.body
        subtitle.textColor = HubPalette.mutedForeground

        let stack = NSStackView(views: [title, subtitle, migrationProgress])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 0
        stack.setCustomSpacing(4, after: title)
        stack.setCustomSpacing(14, after: subtitle)
        return stack
    }

    private func startMigrationProgress() {
        migrationProgressPhase = nil
        migrationProgressCaption.stringValue = migrationProgressCaptionText
        migrationProgress.minValue = 0
        migrationProgress.maxValue = 1
        migrationProgress.doubleValue = 0
    }

    func updateMigrationProgress(_ progress: HubMigrationProgress) {
        guard focusedOperation == .importing else { return }
        if let phase = progress.phase {
            migrationProgressPhase = phase
        }
        migrationProgressCaption.stringValue = migrationProgressCaptionText
        let total = max(1, Double(progress.totalRows))
        let completed = min(Double(progress.completedRows), total)
        let previousTotal = migrationProgress.maxValue
        let previousCompleted = migrationProgress.doubleValue
        migrationProgress.maxValue = total
        migrationProgress.doubleValue = previousTotal == total
            ? min(max(previousCompleted, completed), total)
            : completed
        migrationProgress.setAccessibilityLabel("Import progress")
        migrationProgress.setAccessibilityValue("\(Int(completed)) of \(Int(total)) rows")
    }

    private func resetMigrationProgress() {
        migrationProgressPhase = nil
        migrationProgressCaption.stringValue = migrationProgressCaptionText
        migrationProgress.stopAnimation(nil)
        migrationProgress.minValue = 0
        migrationProgress.maxValue = 1
        migrationProgress.doubleValue = 0
    }

    private var migrationProgressCaptionText: String {
        if migrationProgressPhase == "finalizing" || migrationProgressPhase == "complete" {
            return HubL10n.text("hub.OnboardingWindowController.migration_preparing_history",
                                fallback: "Preparing imported history…")
        }
        return HubL10n.text("hub.OnboardingWindowController.1016.2012",
                            fallback: "Copying your vehicle history into Hub.")
    }

    private func migrationDiagnosticView(_ diagnostic: SourceSSHDiagnostic) -> NSView {
        let title = NSTextField(wrappingLabelWithString: diagnostic.title)
        title.font = HubTypography.emphasis
        title.textColor = HubPalette.foreground

        let summary = NSTextField(wrappingLabelWithString: diagnostic.summary)
        summary.font = HubTypography.body
        summary.maximumNumberOfLines = 0

        let stack = NSStackView(views: [title, summary])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 6
        for suggestion in diagnostic.suggestions {
            let label = NSTextField(wrappingLabelWithString: HubL10n.format("hub.OnboardingWindowController.1070.2015", fallback: "• %1$@", arguments: [String(describing: suggestion)]))
            label.font = HubTypography.body
            label.textColor = .secondaryLabelColor
            label.maximumNumberOfLines = 0
            stack.addArrangedSubview(label)
        }

        var buttons: [NSButton] = []
        for action in diagnostic.recoveryActions {
            let button: NSButton
            switch action {
            case .chooseKey:
                button = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1082.2016", fallback: "Choose Another Key…"), target: self,
                                  action: #selector(chooseMigrationIdentity))
            case .usePassword:
                button = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1085.2017", fallback: "Use Password"), target: self,
                                  action: #selector(useMigrationPassword))
            case .useKey:
                button = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1088.2018", fallback: "Use SSH Key"), target: self,
                                  action: #selector(useMigrationKey))
            case .openLogs:
                button = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1091.2019", fallback: "Open Logs"), target: self, action: #selector(openLogs))
            }
            configureFlatButton(button)
            buttons.append(button)
        }
        let copy = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1096.2020", fallback: "Copy Details"), target: self,
                            action: #selector(copyMigrationDiagnostic))
        configureFlatButton(copy)
        buttons.append(copy)
        for index in stride(from: 0, to: buttons.count, by: 2) {
            let buttonRow = NSStackView(views: Array(buttons[index..<min(index + 2, buttons.count)]))
            buttonRow.spacing = 12
            buttonRow.alignment = .centerY
            stack.addArrangedSubview(buttonRow)
        }
        for view in stack.arrangedSubviews {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        let card = HubCardView()
        card.identifier = NSUserInterfaceItemIdentifier("onboarding.connection-error")
        stack.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 12),
            stack.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -12),
            stack.topAnchor.constraint(equalTo: card.topAnchor, constant: 12),
            stack.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -12)
        ])
        return card
    }

    private func verifyBody() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 0
        if let configuredCarsNote {
            let note = featureRow(configuredCarsNote, "car.side")
            note.identifier = NSUserInterfaceItemIdentifier("onboarding.verify.carsConfigured")
            stack.addArrangedSubview(note)
            stack.setCustomSpacing(12, after: note)
        }
        if busy && checks.isEmpty {
            let row = NSStackView(views: [spinner, NSTextField(labelWithString: HubL10n.text("hub.DiagnosticsWindowController.210.227", fallback: "Running checks…"))])
            row.spacing = 10
            row.alignment = .centerY
            stack.addArrangedSubview(row)
        } else {
            let card = HubCardView()
            let rows = NSStackView()
            rows.orientation = .vertical
            rows.alignment = .leading
            rows.spacing = 0
            rows.translatesAutoresizingMaskIntoConstraints = false
            card.addSubview(rows)
            for (index, check) in checks.enumerated() {
                if index > 0 {
                    let line = HubOnboardingHairlineView()
                    line.heightAnchor.constraint(equalToConstant: 1).isActive = true
                    rows.addArrangedSubview(line)
                    line.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
                }
                rows.addArrangedSubview(verificationRow(check))
            }
            NSLayoutConstraint.activate([
                card.heightAnchor.constraint(equalToConstant: 318),
                rows.leadingAnchor.constraint(equalTo: card.leadingAnchor),
                rows.trailingAnchor.constraint(equalTo: card.trailingAnchor),
                rows.topAnchor.constraint(equalTo: card.topAnchor),
                rows.bottomAnchor.constraint(equalTo: card.bottomAnchor)
            ])
            stack.addArrangedSubview(card)
            card.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    private func verificationRow(_ check: HubOnboardingCheck) -> NSView {
        let icon = NSImageView(image: symbolImage(
            check.passed ? "checkmark.circle" : "xmark.circle",
            description: check.passed ? HubL10n.text("hub.OnboardingWindowController.1165.2025", fallback: "Passed") : HubL10n.text("hub.OnboardingWindowController.1165.2026", fallback: "Failed")
        ))
        icon.image = icon.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 16, weight: .medium)
        )
        icon.contentTintColor = check.passed ? HubPalette.success : HubPalette.danger
        icon.widthAnchor.constraint(equalToConstant: 16).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 16).isActive = true
        let title = NSTextField(labelWithString: check.title)
        title.font = HubTypography.emphasis
        let detail = NSTextField(labelWithString: check.detail)
        detail.font = HubTypography.body
        detail.textColor = HubPalette.mutedForeground
        let copy = NSStackView(views: [title, detail])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 1
        let row = NSStackView(views: [icon, copy])
        row.identifier = NSUserInterfaceItemIdentifier("onboarding.verify.row")
        row.spacing = 11
        row.alignment = .centerY
        row.edgeInsets = NSEdgeInsets(top: 7, left: 14, bottom: 7, right: 14)
        row.heightAnchor.constraint(equalToConstant: 52).isActive = true
        return row
    }

    private func finishBody() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        if state.path == .migration {
            let tookOver = migrationUseLocalHistory.state != .on
                && (controller.sourceTakeoverStatus.isTakenOver || controller.sourceTakeoverStatus == .unknown)
            if tookOver {
                for (identifier, text) in takeoverFinishCopy {
                    let label = NSTextField(wrappingLabelWithString: text)
                    label.identifier = NSUserInterfaceItemIdentifier(identifier)
                    label.font = HubTypography.body
                    label.maximumNumberOfLines = 0
                    stack.addArrangedSubview(label)
                    label.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
                }
            } else if takeoverDeclined {
                let next = NSTextField(wrappingLabelWithString: HubL10n.text(
                    "hub.sourceTakeover.finishSignInNext",
                    fallback: "After Hub starts, sign in with Tesla on the next page so Hub has its own login."))
                next.font = HubTypography.body
                next.maximumNumberOfLines = 0
                stack.addArrangedSubview(next)
                next.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
            }
            let handoverTitle = migrationUseLocalHistory.state == .on
                ? HubL10n.text("hub.OnboardingWindowController.1198.2028", fallback: "I understand collection remains disabled; this Hub serves imported history only")
                : tookOver
                    ? HubL10n.text("hub.sourceTakeover.acknowledgement", fallback: "I will keep the source application stopped")
                    : HubL10n.text("hub.OnboardingWindowController.1199.2029", fallback: "I have disabled Tesla access in the source application to avoid duplicate requests")
            let acknowledgement = NSButton(checkboxWithTitle: "",
                                           target: self,
                                           action: #selector(handoverChanged(_:)))
            acknowledgement.state = handoverAcknowledged ? .on : .off
            acknowledgement.controlSize = .regular
            acknowledgement.setAccessibilityLabel(
                handoverTitle
            )
            let acknowledgementRow = wrappingCheckbox(
                acknowledgement,
                title: handoverTitle
            )
            stack.addArrangedSubview(acknowledgementRow)
            acknowledgementRow.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    private var takeoverFinishCopy: [(String, String)] {
        var copy = [("onboarding.takeover.holds", HubL10n.text("hub.sourceTakeover.keepStopped",
            fallback: "Keep the source application stopped. Running both on one login can lock you out."))]
        if let progress = controller.sourceTakeoverStatus.accountText {
            copy.append(("onboarding.takeover.progress", progress))
        } else {
            copy.append(("onboarding.takeover.progress", HubL10n.text("hub.sourceTakeover.confirmOwnership",
                fallback: "Hub could not confirm the login state. Keep the source application stopped and check again.")))
        }
        if controller.sourceTakeoverStatus == .takenOver(progress: "none_sent") {
            copy.append(("onboarding.takeover.backBefore", HubL10n.text("hub.sourceTakeover.untouched",
                fallback: "No refresh has been sent: the source login is untouched. To go back, sign out in Hub before starting the source application.")))
        } else if controller.sourceTakeoverStatus == .takenOver(progress: "completed") {
            copy.append(("onboarding.takeover.backAfter", HubL10n.text("hub.sourceTakeover.returnAfterRefresh",
                fallback: "To go back, sign out in Hub, start the source application and sign in there again.")))
        }
        return copy
    }

    private func footerView() -> NSView {
        cancelButton.target = self
        cancelButton.action = #selector(cancelPressed)
        cancelButton.identifier = NSUserInterfaceItemIdentifier("onboarding.cancel")
        configureFlatButton(cancelButton)
        cancelButton.controlSize = .regular
        cancelButton.keyEquivalent = "\u{1b}"
        cancelButton.keyEquivalentModifierMask = []
        backButton.target = self
        backButton.action = #selector(backPressed)
        configureFlatButton(backButton)
        backButton.hubStyle = .neutral
        backButton.image = NSImage(systemSymbolName: "chevron.left", accessibilityDescription: "Back")
        backButton.imagePosition = .imageLeading
        backButton.controlSize = .regular
        backWidthConstraint?.isActive = false
        backWidthConstraint = backButton.widthAnchor.constraint(equalToConstant: HubMetrics.actionMinimumWidth)
        backWidthConstraint?.isActive = true
        continueButton.identifier = NSUserInterfaceItemIdentifier("onboarding.continue")
        continueButton.target = self
        continueButton.action = #selector(continuePressed)
        configurePrimaryButton(continueButton)
        continueButton.controlSize = .regular
        continueWidthConstraint?.isActive = false
        continueWidthConstraint = continueButton.widthAnchor.constraint(equalToConstant: 240)
        continueWidthConstraint?.isActive = true
        continueHeightConstraint?.isActive = false
        continueHeightConstraint = continueButton.heightAnchor.constraint(equalToConstant: HubMetrics.onboardingPrimaryHeight)
        continueHeightConstraint?.isActive = true

        footerSpinner.style = .spinning
        footerSpinner.controlSize = .small
        footerSpinner.isDisplayedWhenStopped = false
        footerSpinner.toolTip = HubL10n.text("hub.OnboardingWindowController.1250.2034", fallback: "Hub setup is working")
        let logs = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1251.2035", fallback: "View Logs"), target: self, action: #selector(openLogs))
        configureFlatButton(logs)
        logs.controlSize = .regular
        footerLogsButton = logs
        let footer = NSView()
        let leading = NSStackView(views: [backButton, cancelButton])
        leading.spacing = 8
        leading.alignment = .centerY
        for view in [leading, continueButton, logs] {
            view.translatesAutoresizingMaskIntoConstraints = false
            footer.addSubview(view)
            view.centerYAnchor.constraint(equalTo: footer.centerYAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            footer.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight),
            leading.leadingAnchor.constraint(equalTo: footer.leadingAnchor),
            continueButton.centerXAnchor.constraint(equalTo: footer.centerXAnchor),
            logs.trailingAnchor.constraint(equalTo: footer.trailingAnchor),
            leading.trailingAnchor.constraint(lessThanOrEqualTo: continueButton.leadingAnchor, constant: -8),
            logs.leadingAnchor.constraint(greaterThanOrEqualTo: continueButton.trailingAnchor, constant: 8)
        ])
        return footer
    }

    /// Another owner holds the lifecycle (diagnostics from Logs, background reconciliation)
    /// while this window is idle: the buttons that would start work wait for it.
    private var lifecycleHeldElsewhere: Bool {
        !busy && authWindow == nil && !controller.previewMode && !controller.lifecycleCapabilities.isIdle
    }

    private func updateFooter() {
        let formBlocked = interactionBlocked
        let blocked = interactionBlocked || lifecycleHeldElsewhere
        let focused = focusedOperation != nil
        let importing = focusedOperation == .importing
        cancelButton.isHidden = dismissalPolicy != .accountManagement || signInIdle
        cancelButton.isEnabled = !closeBlocked
        backButton.isHidden = state.route == .welcome
            || state.route == .finish
            || (state.route == .verify && state.path == .newInstallation)
            || importing
            || importedAwaitingTakeover
        backButton.isEnabled = !formBlocked
        footerLogsButton?.isHidden = state.route != .verify || !verificationFinished
        footerLogsButton?.isEnabled = !formBlocked
        continueButton.isHidden = importing
        // The focused operation body owns its status copy. Keep the footer's
        // control label stable so shared chrome does not repeat that status.
        continueButton.title = continueTitle
        continueButton.image = nil
        switch state.route {
        case .fleet:
            continueButton.isEnabled = fleetContinueEnabled && !blocked
        case .migration:
            if migrationUseLocalHistory.state == .on {
                continueButton.isEnabled = localHistoryInputsValid && !blocked
            } else if isPreviewConnectedMigration {
                continueButton.isEnabled = migrationVersionAcknowledgement.state == .on && !blocked
            } else if importedAwaitingTakeover {
                continueButton.isEnabled = !blocked && (migrationSession == nil
                    ? migrationConnectionInputsValid : !sourceEncryptionKey.stringValue.isEmpty)
            } else if migrationSession != nil {
                continueButton.isEnabled = compatibility?.mayImport == true
                    && sourceRunState == .stopped
                    && !sourceEncryptionKey.stringValue.isEmpty && !blocked
            } else {
                continueButton.isEnabled = migrationConnectionInputsValid && !blocked
            }
        case .verify:
            continueButton.isEnabled = verificationFinished && !blocked
        case .finish where state.path == .migration:
            continueButton.isEnabled = handoverAcknowledged && !blocked
                && (migrationUseLocalHistory.state == .on || controller.sourceTakeoverStatus != .unknown)
        default:
            continueButton.isEnabled = !blocked
        }
        if busy {
            spinner.style = .spinning
            spinner.controlSize = .small
            spinner.startAnimation(nil)
            footerSpinner.toolTip = busyMessage ?? HubL10n.text("hub.OnboardingWindowController.1250.2034", fallback: "Hub setup is working")
            if focused {
                footerSpinner.stopAnimation(nil)
            } else {
                footerSpinner.startAnimation(nil)
            }
            if busyOperation == .connecting {
                migrationSpinner.toolTip = HubL10n.text("hub.OnboardingWindowController.1324.2038", fallback: "Connecting securely to the source application")
                migrationSpinner.startAnimation(nil)
            } else {
                migrationSpinner.stopAnimation(nil)
            }
        } else {
            spinner.stopAnimation(nil)
            migrationSpinner.stopAnimation(nil)
            footerSpinner.stopAnimation(nil)
        }
        migrationConnectButton = state.route == .migration ? continueButton : nil
        migrationConnectButton?.isEnabled = continueButton.isEnabled
        for case let control as NSControl in migrationKeyViews {
            control.isEnabled = !formBlocked
        }
        if let migrationConnectButton {
            migrationConnectButton.title = busyOperation == .connecting
                ? (busyMessage ?? BusyOperation.connecting.message) : continueTitle
            updatePrimaryAppearance(migrationConnectButton)
        }
        updatePrimaryAppearance(continueButton)
        backWidthConstraint?.constant = HubMetrics.actionMinimumWidth
        continueWidthConstraint?.constant = 240
        updateWindowCloseAvailability()
        window?.defaultButtonCell = blocked || continueButton.isHidden || !continueButton.isEnabled
            ? nil
            : continueButton.cell as? NSButtonCell
    }

    private var continueTitle: String {
        switch state.route {
        case .welcome: return HubL10n.text("hub.OnboardingWindowController.1354.2041", fallback: "Get Started")
        case .fleet: return fleetContinueTitle
        case .legacy:
            return signInIdle ? HubL10n.text("hub.signIn.done", fallback: "Done")
                : HubL10n.text("hub.MainWindowController.26.1772", fallback: "Connect Tesla")
        case .migration:
            if migrationUseLocalHistory.state == .on {
                return compatibility?.mayImport == true ? HubL10n.text("hub.OnboardingWindowController.1359.2044", fallback: "Import History") : HubL10n.text("hub.OnboardingWindowController.1359.2045", fallback: "Check Snapshot")
            }
            if importedAwaitingTakeover {
                return migrationSession == nil
                    ? HubL10n.text("hub.sourceTakeover.reconnectButton", fallback: "Reconnect to Server")
                    : HubL10n.text("hub.sourceTakeover.retry", fallback: "Retry")
            }
            return isPreviewConnectedMigration || migrationSession != nil
                ? HubL10n.text("hub.OnboardingWindowController.1362.2046", fallback: "Import Data") : HubL10n.text("hub.OnboardingWindowController.1362.2047", fallback: "Connect to Server")
        case .verify:
            return verificationFinished && !checks.isEmpty && checks.allSatisfy(\.passed)
                ? HubL10n.text("hub.OnboardingWindowController.168.1852", fallback: "Continue") : HubL10n.text("hub.DiagnosticsWindowController.21.204", fallback: "Run Again")
        case .finish: return HubL10n.text("hub.HubDashboardView.119.1362", fallback: "Start Hub")
        default: return HubL10n.text("hub.OnboardingWindowController.168.1852", fallback: "Continue")
        }
    }

    @objc private func selectNewInstallation() {
        state.path = .newInstallation
        render()
    }

    @objc private func selectMigration() {
        state.path = .migration
        render()
    }

    @objc private func selectFleet() {
        guard HubFleetOnboarding.isOffered else { return }
        state.provider = .fleet
        render()
    }

    @objc private func selectLegacy() {
        state.provider = .legacy
        render()
    }

    @objc private func backPressed() {
        guard !interactionBlocked else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        errorMessage = nil
        errorFromWindowCheck = false
        signInIdle = false
        if state.route == .fleet { endFleetSignIn() }
        if state.route == .fleet, fleetPreparationFailed != nil {
            // Back from a failed preparation leaves the route; entering it again prepares again.
            fleetPreparationFailed = nil
            state.back()
            render()
            return
        }
        if state.route == .fleet, let previous = fleetStep.previous {
            fleetStep = previous
            fleetNotice = nil
            render()
            return
        }
        state.back()
        render()
    }

    @objc private func cancelPressed() {
        guard let window, windowShouldClose(window) else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        if !closesWindowOnCancel {
            endFleetSignIn()
            resetMigrationProgress()
            closeMigrationSession()
            discardDiscoveryRetry()
            guard !didNotifyDismiss else { return }
            didNotifyDismiss = true
            onDismiss()
            return
        }
        close()
    }

    override func cancelOperation(_ sender: Any?) {
        cancelPressed()
    }

    @objc private func continuePressed() {
        guard !interactionBlocked else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        switch state.route {
        case .welcome, .choose, .provider:
            state.advance()
            errorMessage = nil
            render()
        case .fleet:
            fleetContinue()
        case .legacy:
            if signInIdle {
                // Done closes set-up; the service is not touched and keeps running.
                onComplete(.configured)
            } else {
                configureLegacy()
            }
        case .migration:
            if migrationUseLocalHistory.state == .on {
                if compatibility?.mayImport == true {
                    importLocalHistory()
                } else {
                    checkLocalHistory()
                }
            } else if isPreviewConnectedMigration {
                return
            } else if importedAwaitingTakeover {
                retryTakeover()
            } else if migrationSession != nil {
                importMigration()
            } else {
                checkMigrationCompatibility()
            }
        case .verify:
            if verificationFinished && !checks.isEmpty && checks.allSatisfy(\.passed) {
                state.advance()
                render()
            } else {
                runVerification()
            }
        case .finish:
            finishOnboarding()
        }
    }

    private func configureLegacy() {
        startLegacySignIn()
    }

    @objc private func startLegacySignIn() {
        guard authWindow == nil else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        // The account context is the controller's, and exists before authentication begins.
        let signIn: HubAccountSignIn
        switch controller.beginLegacySignIn() {
        case let .success(context): signIn = context
        case let .failure(error):
            showInlineError(error.localizedDescription)
            return
        }
        accountSignIn = signIn
        // Sign-in needs an open collection window; check it before the Tesla page opens.
        setBusy(true, operation: .setup)
        signIn.requireOpenWindow { [weak self] windowResult in
            guard let self else { signIn.cancel(); return }
            self.setBusy(false)
            switch windowResult {
            case .success:
                self.openTeslaLogin(signIn)
            case let .failure(error):
                signIn.cancel()
                self.accountSignIn = nil
                HubAppLog.shared.record("authentication.refused", category: "account", level: "WARN",
                                        fields: [
                                            "provider": "legacy",
                                            "error_code": HubAppLog.errorCode(error)
                                        ])
                self.showWindowRefusal(error)
            }
        }
    }

    /// A refused window check, by the service's refusal code (never its line): the ordinary
    /// "no open window" is the idle state; every other refusal, a faulted window included, shows
    /// its own line and is never the healthy notice.
    private func showWindowRefusal(_ error: Error) {
        let code = HubController.signInRefusalCode(error)
        if code == "window_closed" {
            errorMessage = nil
            errorFromWindowCheck = false
            applyWindowOutcome(.closed)
        } else {
            if let code, Self.answeringCodes.contains(code) { serviceConfirmedRunning = true }
            if code == "service_not_running" { serviceConfirmedRunning = false }
            signInIdle = false
            showInlineError(error.localizedDescription)
            errorFromWindowCheck = code.map { !Self.quietCodes.contains($0) } ?? false
            updateWindowCloseAvailability()
        }
    }

    /// Refusals that say nothing about the collection window itself.
    private static let quietCodes: Set<String> = ["service_not_running", "control_channel_busy"]

    /// Refusals that come from a service that answered the window check.
    private static let answeringCodes: Set<String> = [
        "window_faulted", "credential_recovery_pending", "sign_in_unavailable"
    ]

    private var closeKeepsRunningText: String {
        HubL10n.text("hub.setup.closeKeepsRunning", fallback: "Closing this window leaves Hub running.")
    }

    /// Asks the running service, read-only, whether a collection window is open, and brings the
    /// sign-in page into line: idle with no open window, the normal page with one, its own line
    /// for a faulted window or any other refusal. Runs when the page shows and when the owner
    /// returns to the window; no timer.
    func refreshCollectionWindowState() {
        // Nothing to ask while no Hub is installed: the first Connect Tesla starts it.
        guard state.route == .legacy, !controller.previewMode,
              !controller.ownerSignInStartsHub else { return }
        guard !windowObservationInFlight else {
            // The outstanding answer may predate this visit or return: ask again once it lands.
            windowObservationPending = true
            return
        }
        windowObservationInFlight = true
        windowObservation &+= 1
        let observation = windowObservation
        controller.observeCollectionWindow { [weak self] outcome in
            guard let self else { return }
            self.windowObservationInFlight = false
            if self.windowObservationPending {
                self.windowObservationPending = false
                self.refreshCollectionWindowState()
                return
            }
            guard observation == self.windowObservation, self.state.route == .legacy,
                  !self.busy, self.authWindow == nil else { return }
            self.applyWindowOutcome(outcome)
        }
    }

    private func applyWindowOutcome(_ outcome: HubController.CollectionWindowOutcome) {
        var line: String?
        var idle = false
        switch outcome {
        case .open:
            serviceConfirmedRunning = true
        case .closed:
            serviceConfirmedRunning = true
            // A held sign-in whose car discovery failed keeps its retry page.
            idle = !controller.canRetryOwnerDiscovery
        case let .refused(code):
            if Self.answeringCodes.contains(code) { serviceConfirmedRunning = true }
            if code == "service_not_running" { serviceConfirmedRunning = false }
            // A service that is not running or is busy is not a window state: the page stays as
            // it is, and Connect Tesla starts or asks again.
            if !Self.quietCodes.contains(code) {
                line = HubController.signInRefusal(code: code).localizedDescription
            }
        }
        let before = (signInIdle, errorMessage)
        signInIdle = idle
        if let line {
            errorMessage = line
            errorFromWindowCheck = true
        } else if errorFromWindowCheck {
            errorMessage = nil
            errorFromWindowCheck = false
        }
        updateWindowCloseAvailability()
        if before.0 != signInIdle || before.1 != errorMessage { render() } else { updateFooter() }
    }

    private func openTeslaLogin(_ signIn: HubAccountSignIn) {
        HubAppLog.shared.record("authentication.started", category: "account",
                                fields: ["provider": "legacy"])
        do {
            let auth = try TeslaAuthWindowController { [weak self] result in
                guard let self else { signIn.cancel(); return }
                self.authWindow = nil
                self.updateWindowCloseAvailability()
                self.updateFooter()
                switch result {
                case let .success(grant):
                    HubAppLog.shared.record("authentication.completed", category: "account",
                                            fields: ["provider": "legacy"])
                    self.setBusy(true, operation: .setup)
                    signIn.configure(grant: grant) { [weak self] setup in
                        self?.accountSignIn = nil
                        self?.setupFinished(setup)
                    }
                case let .failure(error):
                    signIn.cancel()
                    self.accountSignIn = nil
                    if error as? TeslaAuthError == .cancelled {
                        HubAppLog.shared.record("authentication.cancelled", category: "account",
                                                fields: ["provider": "legacy"])
                    } else {
                        HubAppLog.shared.record("authentication.failed", category: "account",
                                                level: "ERROR", fields: [
                                                    "provider": "legacy",
                                                    "error_code": HubAppLog.errorCode(error)
                                                ])
                        self.showInlineError(error.localizedDescription)
                    }
                }
            }
            authWindow = auth
            updateWindowCloseAvailability()
            updateFooter()
            auth.showWindow(nil)
            auth.window?.makeKeyAndOrderFront(nil)
        } catch {
            signIn.cancel()
            accountSignIn = nil
            HubAppLog.shared.record("authentication.failed", category: "account", level: "ERROR",
                                    fields: [
                                        "provider": "legacy",
                                        "error_code": HubAppLog.errorCode(error)
                                    ])
            showInlineError(error.localizedDescription)
        }
    }

    @objc private func retryOwnerDiscovery() {
        guard authWindow == nil, !busy else { return }
        HubAppLog.shared.record("authentication.resumed", category: "account",
                                fields: ["provider": "legacy"])
        setBusy(true, operation: .setup)
        controller.retryOwnerDiscovery { [weak self] result in
            self?.setupFinished(result)
        }
    }

    private func discardDiscoveryRetry() {
        stopObservingDiscoveryRetryExpiry()
        controller.discardOwnerDiscoveryRetry()
    }

    private func stopObservingDiscoveryRetryExpiry() {
        if let discoveryRetryExpiryObserver {
            NotificationCenter.default.removeObserver(discoveryRetryExpiryObserver)
        }
        discoveryRetryExpiryObserver = nil
    }

    /// The controller drops the grant at its deadline by itself; the page only follows,
    /// hiding the retry and asking for a new sign-in. While busy only the page update waits.
    private func observeDiscoveryRetryExpiry() {
        guard discoveryRetryExpiryObserver == nil else { return }
        discoveryRetryExpiryObserver = NotificationCenter.default.addObserver(
            forName: HubController.ownerDiscoveryRetryExpiredNotification, object: controller, queue: .main
        ) { [weak self] _ in
            guard let self else { return }
            self.stopObservingDiscoveryRetryExpiry()
            guard !self.busy else { return }
            self.errorMessage = Self.discoveryRetryExpiredMessage
            self.render()
        }
    }

    private static var discoveryRetryExpiredMessage: String {
        HubL10n.text("hub.signIn.discovery.expired",
                     fallback: "This sign-in can no longer be resumed. Sign in with Tesla again.")
    }

    private func setupFinished(_ result: Result<Void, Error>) {
        resetMigrationProgress()
        setBusy(false)
        switch result {
        case .success:
            if takeoverDeclined && controller.pendingMigrationHandoverPhase == .awaitingSignIn {
                do { try controller.completeImportedHistorySignIn() }
                catch { showInlineError(error.localizedDescription); return }
                state.path = .newInstallation
                takeoverDeclined = false
            }
            stopObservingDiscoveryRetryExpiry()
            configuredCarsNote = controller.lastVehiclesConfigured.map {
                HubL10n.format("hub.signIn.discovery.configured", fallback: "Cars configured: %d", arguments: [$0])
            }
            state.advance()
            checks = []
            verificationFinished = false
            render()
            runVerification()
        case let .failure(error):
            // A failed car discovery is not success: the page stays, with the retry while the
            // controller still holds the sign-in, and asks for a new one once it does not.
            if error is HubOwnerDiscoveryFailure {
                guard controller.canRetryOwnerDiscovery else {
                    stopObservingDiscoveryRetryExpiry()
                    showInlineError(Self.discoveryRetryExpiredMessage)
                    return
                }
                observeDiscoveryRetryExpiry()
            }
            showInlineError(error.localizedDescription)
        }
    }

    private var localHistoryInputsValid: Bool {
        !migrationSource.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && Int64(migrationCarID.stringValue).map { $0 > 0 } == true
            && !migrationPasswordFile.stringValue.isEmpty
            && migrationVersionAcknowledgement.state == .on
    }

    @objc private func localHistorySelectionChanged() {
        closeMigrationSession()
        compatibility = nil
        connectedMigrationIdentity = nil
        migrationVersionAcknowledgement.state = .off
        render()
    }

    private func checkLocalHistory() {
        guard localHistoryInputsValid else { return }
        setBusy(true, operation: .checkingCompatibility)
        controller.checkSourceCompatibility(source: migrationSource.stringValue,
            carID: migrationCarID.stringValue, passwordFile: migrationPasswordFile.stringValue,
            acceptSchemaEvidence: true, historyOnly: true) { [weak self] result in
            guard let self else { return }
            self.setBusy(false)
            switch result {
            case let .success(report):
                self.compatibility = report
            case let .failure(error):
                self.showInlineError(error.localizedDescription)
            }
            self.render()
        }
    }

    private func importLocalHistory() {
        guard localHistoryInputsValid, compatibility?.mayImport == true else { return }
        startMigrationProgress()
        setBusy(true, operation: .importing)
        controller.importSourceHistoryOnlyOnline(source: migrationSource.stringValue,
            carID: migrationCarID.stringValue, passwordFile: migrationPasswordFile.stringValue,
            acceptSchemaEvidence: true,
            progress: { [weak self] update in self?.updateMigrationProgress(update) }) {
                [weak self] result in self?.setupFinished(result)
            }
    }

    @objc private func checkMigrationCompatibility() {
        let host = migrationServer.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let user = migrationUser.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let port = Int(migrationPort.stringValue), (1...65535).contains(port),
              !host.isEmpty, !user.isEmpty else {
            migrationSSHPassword.stringValue = ""
            HubAppLog.shared.record("compatibility.rejected", category: "history_import",
                                    level: "WARN", fields: ["reason": "missing_server_input"])
            showInlineError(HubL10n.text("hub.OnboardingWindowController.1631.2105", fallback: "Enter the source application server, SSH user, and port."))
            return
        }
        let passwordAuthentication = migrationAuthentication.indexOfSelectedItem != 0
        guard !passwordAuthentication || !migrationSSHPassword.stringValue.isEmpty else {
            showInlineError(HubL10n.text("hub.sourceTakeover.sshPasswordRequired",
                fallback: "Enter the SSH password again to connect to the source server."))
            return
        }
        let authentication: SourceSSHAuthentication
        if passwordAuthentication { authentication = .password(migrationSSHPassword.stringValue) }
        else {
            let path = migrationIdentityFile.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            authentication = .key(identityFile: path.isEmpty ? nil : URL(fileURLWithPath: path))
        }
        migrationSSHPassword.stringValue = ""
        HubAppLog.shared.record("compatibility.started", category: "history_import")
        setBusy(true, operation: .connecting)
        let requestedMigrationIdentity = currentMigrationIdentity
        compatibility = nil
        migrationDiagnostic = nil
        errorMessage = nil
        migrationVersionAcknowledgement.state = .off
        closeMigrationSession()
        connectedMigrationIdentity = nil
        migrationPreparation = controller.connectMigrationServer(
            host: host,
            user: user,
            port: port,
            authentication: authentication,
            usePasswordlessSudo: migrationUseSudo.state == .on) { [weak self, controller] result in
            guard let self else {
                // No window is left to use the connection: the controller closes it.
                if case let .success(session) = result { controller.closeMigrationSession(session) }
                return
            }
            self.migrationPreparation = nil
            switch result {
            case let .success(session):
                self.migrationDiagnostic = nil
                self.migrationSSHPassword.stringValue = ""
                self.migrationSession = session
                self.connectedMigrationIdentity = requestedMigrationIdentity
                self.migrationSource.stringValue = session.source
                self.migrationCarID.stringValue = session.carID
                self.migrationPasswordFile.stringValue = session.passwordFile.path
                if self.importedAwaitingTakeover {
                    self.setBusy(false)
                    self.runSourceStopCheck(session)
                } else { self.checkConnectedMigrationSession(session) }
            case let .failure(error):
                HubAppLog.shared.record("connection.failed", category: "history_import",
                                        level: "ERROR",
                                        fields: ["error_code": HubAppLog.errorCode(error)])
                self.setBusy(false)
                if case HubActionError.lifecycleRefused = error {
                    // Not a connection problem: the Hub was busy and nothing was started.
                    self.showInlineError(error.localizedDescription)
                    return
                }
                self.compatibility = nil
                self.errorMessage = nil
                self.migrationDiagnostic = SourceServerImporter.connectionDiagnostic(
                    for: error,
                    authentication: passwordAuthentication ? .password("") : .key(identityFile: nil)
                )
                self.render()
            }
        }
    }

    private func checkConnectedMigrationSession(_ session: SourceServerImportSession) {
        guard session === migrationSession else { return }
        let versionAccepted = migrationVersionAcknowledgement.state == .on
        guard versionAccepted else {
            setBusy(false)
            compatibility = HubSourceCheckResult(
                compatible: false,
                message: HubL10n.text("hub.OnboardingWindowController.1691.2113", fallback: "A schema check cannot establish the source application release. Confirm that the running server is at release 4.4.0 or later, then continue."),
                reasonCode: "v4_4_version_unconfirmed",
                requiredVersion: HubSourceCheckResult.requiredSourceRelease
            )
            render()
            return
        }
        controller.checkSourceCompatibility(
            source: session.source,
            carID: session.carID,
            passwordFile: session.passwordFile.path,
            acceptSchemaEvidence: versionAccepted
        ) { [weak self] check in
            guard let self else { return }
            self.setBusy(false)
            switch check {
            case let .success(report):
                HubAppLog.shared.record("compatibility.completed",
                                        category: "history_import",
                                        fields: [
                                            "compatible": report.compatible ? "true" : "false",
                                            "reason": report.reasonCode
                                        ])
                self.compatibility = report
                self.errorMessage = nil
                if report.mayImport {
                    self.render()
                    self.runSourceStopCheck(session)
                    return
                }
            case let .failure(error):
                HubAppLog.shared.record("compatibility.failed", category: "history_import",
                                        level: "ERROR",
                                        fields: ["error_code": HubAppLog.errorCode(error)])
                self.closeMigrationSession()
                self.connectedMigrationIdentity = nil
                self.compatibility = HubSourceCheckResult(compatible: false,
                                                               message: error.localizedDescription,
                                                               reasonCode: "unavailable",
                                                               requiredVersion: HubSourceCheckResult.requiredSourceRelease)
            }
            self.render()
        }
    }

    private func importMigration() {
        let versionAccepted = migrationVersionAcknowledgement.state == .on
        guard let session = migrationSession,
              migrationInputsComplete,
              versionAccepted,
              compatibility?.mayImport == true else {
            HubAppLog.shared.record("import.rejected", category: "history_import",
                                    level: "WARN", fields: ["reason": "connection_not_ready"])
            showInlineError(HubL10n.text("hub.OnboardingWindowController.1740.2131", fallback: "Connect to the source application server before importing."))
            return
        }
        guard connectedMigrationIdentity == currentMigrationIdentity else {
            HubAppLog.shared.record("import.rejected", category: "history_import",
                                    level: "WARN", fields: ["reason": "settings_changed"])
            closeMigrationSession()
            connectedMigrationIdentity = nil
            compatibility = nil
            showInlineError(HubL10n.text("hub.OnboardingWindowController.1750.2137", fallback: "Server settings changed. Connect to the source application server again before importing."))
            return
        }
        guard !sourceEncryptionKey.stringValue.isEmpty else {
            showInlineError(HubL10n.text("hub.sourceTakeover.keyRequired", fallback: "Enter the source application's encryption key."))
            return
        }
        // The stop gate runs again right before the import: the import starts only once the
        // source application is seen stopped.
        runSourceStopCheck(session) { [weak self] stopped in
            guard let self, stopped, session === self.migrationSession else { return }
            self.startImport(session, versionAccepted: versionAccepted)
        }
    }

    private func startImport(_ session: SourceServerImportSession, versionAccepted: Bool) {
        startMigrationProgress()
        setBusy(true, operation: .importing)
        controller.importSourceOnline(source: session.source,
                                         carID: session.carID,
                                         passwordFile: session.passwordFile.path,
                                         acceptSchemaEvidence: versionAccepted,
                                         awaitingTakeover: true,
                                         progress: { [weak self] update in
                                             self?.updateMigrationProgress(update)
                                         }) { [weak self, controller] result in
            guard let self, self.migrationSession === session else {
                // No window is left to run the takeover: the controller closes the session.
                controller.closeMigrationSession(session)
                return
            }
            switch result {
            case .success:
                // The session stays open for the takeover and its Retry.
                self.resetMigrationProgress()
                self.importedAwaitingTakeover = true
                self.runTakeover(session)
            case .failure:
                controller.closeMigrationSession(session)
                self.migrationSession = nil
                self.connectedMigrationIdentity = nil
                self.sourceEncryptionKey.stringValue = ""
                self.setupFinished(result)
            }
        }
    }

    /// Runs the stop check of the connected session and shows its answer; `completion`
    /// hears true only when the source application was seen stopped.
    private func runSourceStopCheck(_ session: SourceServerImportSession,
                                    completion: @escaping (Bool) -> Void = { _ in }) {
        let host = migrationServer.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let user = migrationUser.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let port = Int(migrationPort.stringValue) ?? 0
        sourceRunState = nil
        sourceStopCheckError = nil
        errorMessage = nil
        setBusy(true, operation: .checkingSourceStopped)
        render()
        controller.checkSourceStopped(session: session, host: host, user: user, port: port,
                                      usePasswordlessSudo: migrationUseSudo.state == .on) { [weak self] result in
            guard let self else { return }
            guard session === self.migrationSession else { completion(false); return }
            self.setBusy(false)
            switch result {
            case let .success(state):
                self.sourceRunState = state
            case let .failure(error):
                if case HubActionError.lifecycleRefused = error {
                    self.showInlineError(error.localizedDescription)
                    completion(false)
                    return
                }
                self.closeMigrationSession()
                self.connectedMigrationIdentity = nil
                self.sourceStopCheckError = HubActionError.settingQuiescenceAside(error).localizedDescription
                self.errorMessage = self.sourceStopCheckError
            }
            self.render()
            completion(self.sourceRunState == .stopped)
        }
    }

    @objc private func checkSourceStoppedPressed() {
        guard !interactionBlocked, let session = migrationSession else { return }
        runSourceStopCheck(session)
    }

    /// Recheck immediately before every send, including after the history import.
    private func runTakeover(_ session: SourceServerImportSession) {
        runSourceStopCheck(session) { [weak self] stopped in
            guard let self else { return }
            guard stopped, session === self.migrationSession else {
                self.sourceEncryptionKey.stringValue = ""
                self.render()
                return
            }
            self.sendTakeover(session)
        }
    }

    private func sendTakeover(_ session: SourceServerImportSession) {
        // Read only for the synchronous serialisation. Framework strings cannot be wiped.
        defer { sourceEncryptionKey.stringValue = "" }
        takeoverFailure = nil
        errorMessage = nil
        setBusy(true, operation: .takingOver)
        controller.takeOverSourceToken(session: session, encryptionKey: sourceEncryptionKey.stringValue) { [weak self, controller] result in
            guard let self, self.migrationSession === session else {
                controller.closeMigrationSession(session)
                return
            }
            self.sourceEncryptionKey.stringValue = ""
            switch result {
            case .success:
                self.importedAwaitingTakeover = false
                self.closeMigrationSession()
                self.connectedMigrationIdentity = nil
                self.setupFinished(.success(()))
            case let .failure(error):
                self.setBusy(false)
                let shown = HubActionError.settingQuiescenceAside(error)
                self.takeoverFailure = shown as? HubSourceTakeoverRefusal
                    ?? HubSourceTakeoverRefusal(code: "takeover_failed", message: shown.localizedDescription)
                self.render()
            }
        }
        render()
    }

    @objc private func reconnectTakeover() {
        guard importedAwaitingTakeover, !interactionBlocked else { return }
        closeMigrationSession()
        connectedMigrationIdentity = nil
        render()
    }

    private func retryTakeover() {
        guard importedAwaitingTakeover else { return }
        if let session = migrationSession {
            guard !sourceEncryptionKey.stringValue.isEmpty else { return }
            runTakeover(session)
        } else { checkMigrationCompatibility() }
    }

    /// Reconcile before declining: a lost reply must never offer a second login over a
    /// committed takeover. Persist only the missing step; the imported history stays put.
    @objc private func declineTakeover() {
        guard importedAwaitingTakeover, !interactionBlocked else { return }
        sourceEncryptionKey.stringValue = ""
        setBusy(true, operation: .takingOver)
        controller.reconcileSourceTakeover { [weak self] result in
            guard let self else { return }
            self.setBusy(false)
            guard case .success = result else {
                if case let .failure(error) = result { self.showInlineError(error.localizedDescription) }
                return
            }
            do {
                if self.controller.sourceTakeoverStatus.hasLogin {
                    try self.controller.advanceTakeoverPhase(to: .awaitingVerification)
                    self.importedAwaitingTakeover = false
                    self.closeMigrationSession()
                    self.setupFinished(.success(()))
                } else if self.controller.sourceTakeoverStatus == .absent {
                    try self.controller.advanceTakeoverPhase(to: .awaitingSignIn)
                    self.importedAwaitingTakeover = false
                    self.takeoverFailure = nil
                    self.takeoverDeclined = true
                    self.closeMigrationSession()
                    self.connectedMigrationIdentity = nil
                    self.state.route = .legacy
                    self.render()
                    self.refreshCollectionWindowState()
                } else {
                    self.showInlineError(HubSourceTakeoverRefusal.message(code: "slot_occupied"))
                }
            } catch { self.showInlineError(error.localizedDescription) }
        }
    }

    private var migrationInputsComplete: Bool {
        !migrationSource.stringValue.isEmpty
            && Int64(migrationCarID.stringValue).map { $0 > 0 } == true
            && !migrationPasswordFile.stringValue.isEmpty
    }

    private var currentMigrationIdentity: String {
        let values = [
            migrationServer.stringValue.trimmingCharacters(in: .whitespacesAndNewlines),
            migrationUser.stringValue.trimmingCharacters(in: .whitespacesAndNewlines),
            migrationPort.stringValue.trimmingCharacters(in: .whitespacesAndNewlines),
            String(migrationAuthentication.indexOfSelectedItem),
            migrationIdentityFile.stringValue.trimmingCharacters(in: .whitespacesAndNewlines),
            migrationUseSudo.state == .on ? "sudo" : "direct"
        ]
        return values.joined(separator: "\u{0}")
    }

    private var migrationConnectionInputsValid: Bool {
        let host = migrationServer.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let user = migrationUser.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        return !host.isEmpty
            && !user.isEmpty
            && Int(migrationPort.stringValue).map { (1...65535).contains($0) } == true
            && (migrationAuthentication.indexOfSelectedItem == 0 || !migrationSSHPassword.stringValue.isEmpty)
    }

    private func runVerification() {
        checks = []
        verificationFinished = false
        busy = true
        busyOperation = .runningChecks
        busyMessage = BusyOperation.runningChecks.message
        errorMessage = nil
        render()
        controller.runOnboardingChecks(expectRunning: state.path == .newInstallation) { [weak self] result in
            DispatchQueue.main.async {
                guard let self else { return }
                self.setBusy(false)
                self.verificationFinished = true
                switch result {
                case let .success(checks):
                    self.checks = checks
                    self.errorMessage = checks.allSatisfy(\.passed) ? nil : HubL10n.text("hub.OnboardingWindowController.1820.2143", fallback: "One or more checks need attention.")
                case let .failure(error):
                    if case HubActionError.lifecycleRefused(.busy(.reconcile)) = error {
                        // Launch-time reconciliation is short and runs by itself: verification
                        // waits for it instead of reporting a failure.
                        self.controller.lifecycle.whenIdle {
                            DispatchQueue.main.async { [weak self] in self?.runVerification() }
                        }
                        return
                    }
                    self.checks = []
                    self.errorMessage = error.localizedDescription
                }
                self.render()
            }
        }
    }

    private func finishOnboarding() {
        guard state.path == .migration else {
            onComplete(.configured)
            return
        }
        guard handoverAcknowledged else { return }
        setBusy(true, operation: .starting)
        controller.acknowledgeMigrationHandoverAndStart { [weak self] result in
            guard let self else { return }
            self.setBusy(false)
            switch result {
            case .success: self.onComplete(.hubStarted)
            case let .failure(error): self.showInlineError(error.localizedDescription)
            }
        }
    }

    @objc private func handoverChanged(_ sender: NSButton) {
        handoverAcknowledged = sender.state == .on
        updateFooter()
    }

    @objc private func openFleetGuide() {
        if let url = URL(string: "https://developer.tesla.com") {
            NSWorkspace.shared.open(url)
        }
    }

    @objc private func openLogs() {
        guard !interactionBlocked, let window else { return }
        logsReturnResponder = normalizedResponderView()
        let logs = LogsWindowController(controller: controller, embedded: true)
        logsWindow = logs
        let page = logs.makeEmbeddedPage { [weak self] in
            guard let self, let window = self.window else { return }
            window.contentView = self.onboardingContainer
            self.logsWindow = nil
            self.updateFooter()
            window.recalculateKeyViewLoop()
            self.configureKeyViewLoop(previousField: self.logsReturnResponder)
            self.logsReturnResponder = nil
            HubMotion.transition(self.onboardingContainer, forward: false)
        }
        window.contentView = page
        window.defaultButtonCell = nil
        (page as? HubEmbeddedUtilityPage)?.focusInitialResponder(in: window)
        HubMotion.transition(page, forward: true)
    }

    private func normalizedResponderView() -> NSView? {
        if let editor = window?.firstResponder as? NSTextView,
           let delegate = editor.delegate as? NSView {
            return delegate
        }
        return window?.firstResponder as? NSView
    }

    @objc private func chooseMigrationPassword() {
        chooseFile(for: migrationPasswordFile)
    }

    @objc private func chooseMigrationIdentity() {
        chooseFile(for: migrationIdentityFile)
    }

    @objc private func useMigrationPassword() {
        migrationAuthentication.selectItem(at: 1)
        migrationAuthenticationChanged()
        window?.makeFirstResponder(migrationSSHPassword)
    }

    @objc private func useMigrationKey() {
        migrationAuthentication.selectItem(at: 0)
        migrationAuthenticationChanged()
        window?.makeFirstResponder(migrationIdentityFile)
    }

    @objc private func copyMigrationDiagnostic() {
        guard let migrationDiagnostic else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(migrationDiagnostic.safeReport, forType: .string)
    }

    @objc private func migrationAuthenticationChanged() {
        compatibility = nil
        errorMessage = nil
        migrationDiagnostic = nil
        migrationVersionAcknowledgement.state = .off
        closeMigrationSession()
        connectedMigrationIdentity = nil
        render()
    }

    func controlTextDidChange(_ notification: Notification) {
        guard let field = notification.object as? NSTextField else { return }
        if field === fleetClientID || field === fleetClientSecret
            || field === fleetRedirectURI || field === fleetDomain {
            updateFooter()
            return
        }
        if field === sourceEncryptionKey || field === migrationSSHPassword {
            updateFooter()
            return
        }
        if field === migrationSource || field === migrationCarID || field === migrationPasswordFile {
            compatibility = nil
            updateFooter()
            return
        }
        guard field === migrationServer || field === migrationUser || field === migrationPort else { return }
        updateFooter()
    }

    @objc private func changeMigrationServer() {
        closeMigrationSession()
        connectedMigrationIdentity = nil
        compatibility = nil
        migrationVersionAcknowledgement.state = .off
        errorMessage = nil
        render()
    }

    @objc private func migrationVersionAcknowledgementChanged() {
        if migrationUseLocalHistory.state == .on {
            compatibility = nil
            render()
            return
        }
        guard let session = migrationSession else {
            render()
            return
        }
        compatibility = nil
        if migrationVersionAcknowledgement.state == .on {
            setBusy(true, operation: .checkingCompatibility)
            checkConnectedMigrationSession(session)
        } else {
            render()
        }
    }

    private func chooseFile(for field: NSTextField) {
        guard let window, !interactionBlocked, !filePanelOpen else { return }
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        if field === migrationIdentityFile {
            panel.title = HubL10n.text("hub.OnboardingWindowController.1980.2147", fallback: "Choose SSH Key")
            panel.prompt = HubL10n.text("hub.OnboardingWindowController.1981.2148", fallback: "Choose Key")
            panel.showsHiddenFiles = true
            panel.directoryURL = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".ssh")
        }
        filePanelOpen = true
        presentFilePanel(panel, window) { [weak self, weak field] url in
            guard let self else { return }
            self.filePanelOpen = false
            guard let field, let url else { return }
            field.stringValue = url.path
            field.toolTip = url.path
            if field === self.migrationPasswordFile {
                self.compatibility = nil
            }
            self.updateFooter()
        }
    }

    func setBusy(_ value: Bool, message: String? = nil, operation: BusyOperation? = nil) {
        let wasFocused = focusedOperation != nil
        let wasBusy = busy
        let previousMessage = busyMessage
        if value, !wasBusy, case .importing? = operation {
            migrationProgressPhase = nil
            migrationProgressCaption.stringValue = migrationProgressCaptionText
        }
        busy = value
        busyOperation = value ? operation : nil
        busyMessage = value ? (message ?? operation?.message) : nil
        updateWindowCloseAvailability()
        if wasFocused || focusedOperation != nil {
            render()
        } else {
            updateFooter()
        }
        let announcementSource: Any = onboardingContainer.map { $0 as Any } ?? self
        if value, !wasBusy {
            HubAccessibility.announce(busyMessage ?? "Setup operation started.", from: announcementSource)
        } else if !value, wasBusy {
            HubAccessibility.announce("\(previousMessage ?? "Setup operation") finished.",
                                      from: announcementSource)
        }
    }

    private func updateWindowCloseAvailability() {
        let close = window?.standardWindowButton(.closeButton)
        close?.isEnabled = canCancel
        close?.toolTip = canCancel && serviceConfirmedRunning ? closeKeepsRunningText : nil
    }

    private var interactionBlocked: Bool { busy || authWindow != nil }

    private var closeBlocked: Bool { interactionBlocked || controller.hasPendingMigrationHandover }

    private func showInlineError(_ message: String) {
        setBusy(false)
        errorMessage = message
        render()
        let announcementSource: Any = onboardingContainer.map { $0 as Any } ?? self
        HubAccessibility.announce("Setup failed: \(message)", from: announcementSource)
    }

    private func withError(_ view: NSView) -> NSView {
        guard let errorMessage else { return view }
        let error = noticeView(symbol: "exclamationmark.triangle.fill", tone: .warning, title: nil,
                               message: errorMessage, identifier: "onboarding.notice.error")
        let stack = NSStackView(views: [view, error])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        error.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        return stack
    }

    /// The app's notice: a card with a status icon, an optional heading and the message.
    private func noticeView(symbol: String, tone: HubStatusTone, title: String?,
                            message: String, identifier: String) -> NSView {
        let icon = NSImageView(image: symbolImage(symbol, description: nil))
        icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 18, weight: .medium)
        icon.contentTintColor = tone.color
        icon.setAccessibilityElement(false)
        icon.setContentHuggingPriority(.required, for: .horizontal)
        icon.widthAnchor.constraint(equalToConstant: 20).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 20).isActive = true
        var labels: [NSTextField] = []
        if let title {
            let heading = NSTextField(wrappingLabelWithString: title)
            heading.font = HubTypography.emphasis
            heading.textColor = HubPalette.foreground
            heading.maximumNumberOfLines = 0
            labels.append(heading)
        }
        let body = NSTextField(wrappingLabelWithString: message)
        body.font = HubTypography.body
        body.textColor = title == nil ? HubPalette.foreground : HubPalette.mutedForeground
        body.maximumNumberOfLines = 0
        labels.append(body)
        let copy = NSStackView(views: labels)
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 3
        for label in labels { label.widthAnchor.constraint(equalTo: copy.widthAnchor).isActive = true }
        let row = NSStackView(views: [icon, copy])
        row.alignment = .top
        row.spacing = 12
        row.translatesAutoresizingMaskIntoConstraints = false
        let card = HubCardView()
        card.identifier = NSUserInterfaceItemIdentifier(identifier)
        card.addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            row.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            row.topAnchor.constraint(equalTo: card.topAnchor, constant: 12),
            row.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -12)
        ])
        return card
    }

    private func verticalChoices(_ choices: [NSView]) -> NSView {
        let stack = NSStackView(views: choices)
        stack.orientation = .vertical
        stack.spacing = 8
        stack.alignment = .leading
        for choice in choices {
            choice.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return stack
    }

    private func choiceButton(title: String,
                              subtitle: String,
                              selected: Bool,
                              action: Selector) -> NSView {
        let card = HubCardView()
        card.isSelected = selected
        let symbol: String
        switch action {
        case #selector(selectNewInstallation): symbol = "cylinder"
        case #selector(selectMigration): symbol = "square.and.arrow.down"
        case #selector(selectFleet): symbol = "antenna.radiowaves.left.and.right"
        default: symbol = "key"
        }
        let icon = HubIconTileView(symbol: symbol, accessibilityDescription: nil)
        let label = NSTextField(labelWithString: title)
        label.font = HubTypography.emphasis
        let detail = NSTextField(wrappingLabelWithString: subtitle)
        detail.font = HubTypography.caption
        detail.textColor = .secondaryLabelColor
        detail.maximumNumberOfLines = 2
        let copy = NSStackView(views: [label, detail])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 3
        let indicator = HubIconTileView(symbol: selected ? "largecircle.fill.circle" : "circle",
                                        accessibilityDescription: nil, size: 18, symbolSize: 14,
                                        tint: selected ? HubPalette.accent : .secondaryLabelColor)
        let row = NSStackView(views: [icon, copy, spacer(), indicator])
        row.alignment = .centerY
        row.spacing = 12
        row.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(row)
        let radio = HubChoiceRowButton(radioButtonWithTitle: title, target: self, action: action)
        radio.state = selected ? .on : .off
        radio.isTransparent = true
        // The selected card shows its own highlight; the transparent radio draws no ring of its own.
        radio.focusRingType = .none
        radio.setAccessibilityHelp(subtitle)
        radio.setAccessibilityValue(selected ? "Selected" : "Not selected")
        radio.translatesAutoresizingMaskIntoConstraints = false
        pageKeyViews.append(radio)
        card.addSubview(radio)
        NSLayoutConstraint.activate([
            card.heightAnchor.constraint(equalToConstant: 64),
            row.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 16),
            row.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -16),
            row.centerYAnchor.constraint(equalTo: card.centerYAnchor),
            radio.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            radio.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            radio.topAnchor.constraint(equalTo: card.topAnchor),
            radio.bottomAnchor.constraint(equalTo: card.bottomAnchor)
        ])
        return card
    }

    private func verticalField(_ title: String, _ field: NSView, width: CGFloat? = nil) -> NSView {
        let label = NSTextField(labelWithString: title)
        label.font = HubTypography.label
        label.textColor = HubPalette.mutedForeground
        if let width {
            field.widthAnchor.constraint(equalToConstant: width).isActive = true
        }
        if let control = field as? NSControl {
            control.controlSize = .regular
            control.font = HubTypography.body
        }
        let stack = NSStackView(views: [label, field])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 4
        if width == nil {
            field.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return stack
    }

    private func wrappingCheckbox(_ checkbox: NSButton, title: String) -> NSView {
        let label = NSTextField(wrappingLabelWithString: title)
        label.font = HubTypography.body
        label.maximumNumberOfLines = 0
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let row = NSStackView(views: [checkbox, label])
        row.alignment = .firstBaseline
        row.spacing = 7
        return row
    }

    private func featureRow(_ title: String,
                            _ symbol: String,
                            color: NSColor = .secondaryLabelColor) -> NSView {
        let image = NSImageView(image: symbolImage(symbol, description: nil))
        image.setAccessibilityElement(false)
        image.image = image.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 24, weight: .medium)
        )
        image.contentTintColor = color
        image.widthAnchor.constraint(equalToConstant: 18).isActive = true
        image.heightAnchor.constraint(equalToConstant: 18).isActive = true
        let label = NSTextField(wrappingLabelWithString: title)
        label.font = HubTypography.body
        label.maximumNumberOfLines = 2
        let row = NSStackView(views: [image, label])
        row.spacing = 12
        row.alignment = .centerY
        return row
    }

    private func configureKeyViewLoop(previousField: NSView?) {
        guard let window else { return }
        var candidates = pageKeyViews
        switch state.route {
        case .fleet:
            candidates.append(contentsOf: fleetKeyViews)
        case .legacy:
            break
        case .migration:
            candidates.append(contentsOf: migrationKeyViews)
        case .welcome, .choose, .provider, .verify, .finish:
            break
        }
        var keyViews = candidates.filter { view in
            !view.isHidden && (view as? NSControl)?.isEnabled != false
        }
        if !backButton.isHidden && backButton.isEnabled { keyViews.append(backButton) }
        if !cancelButton.isHidden && cancelButton.isEnabled { keyViews.append(cancelButton) }
        if !continueButton.isHidden && continueButton.isEnabled { keyViews.append(continueButton) }
        if let footerLogsButton, !footerLogsButton.isHidden, footerLogsButton.isEnabled {
            keyViews.append(footerLogsButton)
        }
        guard !keyViews.isEmpty else { return }
        for index in keyViews.indices {
            keyViews[index].nextKeyView = keyViews[(index + 1) % keyViews.count]
        }
        guard let responder = previousField.flatMap({ previous in
            keyViews.first { $0 === previous }
        }) ?? keyViews.first, responder.window === window else { return }
        window.initialFirstResponder = responder
        window.makeFirstResponder(responder)
    }

    private func formRow(_ title: String, _ field: NSView) -> NSView {
        let label = NSTextField(labelWithString: title)
        label.font = HubTypography.body
        label.textColor = HubPalette.foreground
        label.widthAnchor.constraint(equalToConstant: HubMetrics.formLabelWidth).isActive = true
        field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        field.setContentHuggingPriority(.defaultLow, for: .horizontal)
        field.widthAnchor.constraint(equalToConstant:
            HubMetrics.onboardingContentWidth
                - (HubMetrics.rowHorizontalInset * 2)
                - HubMetrics.formLabelWidth
                - 16
        ).isActive = true
        let row = NSStackView(views: [label, field])
        row.spacing = 16
        row.alignment = .centerY
        row.distribution = .fill
        row.edgeInsets = NSEdgeInsets(top: 10, left: HubMetrics.rowHorizontalInset,
                                     bottom: 10, right: HubMetrics.rowHorizontalInset)
        row.heightAnchor.constraint(equalToConstant: HubMetrics.rowHeight).isActive = true
        return row
    }

    private func onboardingForm(_ fields: [(String, NSView)]) -> NSView {
        let card = HubCardView()
        let rows = NSStackView()
        rows.orientation = .vertical
        rows.spacing = 0
        rows.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(rows)
        for (index, item) in fields.enumerated() {
            if index > 0 {
                let line = HubOnboardingHairlineView()
                rows.addArrangedSubview(line)
                line.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
            }
            let row = formRow(item.0, item.1)
            rows.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            rows.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            rows.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            rows.topAnchor.constraint(equalTo: card.topAnchor),
            rows.bottomAnchor.constraint(equalTo: card.bottomAnchor)
        ])
        return card
    }

    private func fieldWithButton(_ field: NSTextField,
                                 _ button: NSButton,
                                 symbol: String = "folder") -> NSView {
        configureFlatButton(button, symbol: symbol)
        let row = NSStackView(views: [field, button])
        row.spacing = 8
        return row
    }

    private func configureFlatButton(_ button: NSButton,
                                     symbol: String? = nil,
                                     tint: NSColor = .labelColor) {
        button.isBordered = false
        button.image = symbol.flatMap {
            NSImage(systemSymbolName: $0, accessibilityDescription: button.title)
        }
        button.imagePosition = .imageLeading
        button.contentTintColor = .labelColor
        (button as? HubActionButton)?.hubStyle = .flat
        button.font = HubTypography.action
        (button as? HubActionButton)?.hubFont = HubTypography.action
        button.focusRingType = .default
    }

    private func configurePrimaryButton(_ button: NSButton, symbol: String? = nil) {
        button.isBordered = false
        (button as? HubActionButton)?.hubStyle = .primary
        button.image = symbol.flatMap {
            NSImage(systemSymbolName: $0, accessibilityDescription: button.title)
        }
        button.imagePosition = .imageTrailing
        button.contentTintColor = .white
        button.font = HubTypography.action
        (button as? HubActionButton)?.hubFont = HubTypography.action
        button.keyEquivalent = "\r"
        updatePrimaryAppearance(button)
    }

    private func updatePrimaryAppearance(_ button: NSButton) {
        if let button = button as? HubActionButton {
            button.updateHubAppearance()
        }
    }

    private func centered(_ view: NSView) -> NSView {
        let row = NSStackView(views: [spacer(), view, spacer()])
        row.alignment = .centerY
        return row
    }

    private func separator() -> NSBox {
        let line = NSBox()
        line.boxType = .separator
        return line
    }

    private func spacer() -> NSView {
        let view = NSView()
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        return view
    }

    private func appIcon() -> NSImage {
        if let url = Bundle.main.url(forResource: "AppIcon", withExtension: "icns"),
           let image = NSImage(contentsOf: url) {
            return image
        }
        return NSApplication.shared.applicationIconImage
    }

    private func symbolImage(_ name: String, description: String?) -> NSImage {
        if let image = NSImage(systemSymbolName: name, accessibilityDescription: description) {
            return image
        }
        let fallback = NSImage(named: NSImage.infoName) ?? NSImage(size: NSSize(width: 16, height: 16))
        fallback.accessibilityDescription = description
        return fallback
    }
}

// MARK: - Fleet route (M09-T12)

extension OnboardingWindowController {
    // The cadence, price and cap the consent page states: exactly the figures the service
    // states (`setup-fleet figures`, `HubController.readFleetFigures`), never fixed text.

    /// "every 30 seconds", "every 2 minutes", "every hour".
    static func fleetEvery(_ seconds: Int) -> String {
        if seconds % 3_600 == 0 {
            return HubL10n.plural("hub.fleetSetup.every.hours", count: seconds / 3_600, one: "every hour", other: "every %d hours")
        }
        if seconds % 60 == 0 {
            return HubL10n.plural("hub.fleetSetup.every.minutes", count: seconds / 60, one: "every minute", other: "every %d minutes")
        }
        return HubL10n.plural("hub.fleetSetup.every.seconds", count: seconds, one: "every second", other: "every %d seconds")
    }

    /// "$10.00" for 1,000 cents.
    static func fleetDollars(cents: Int) -> String {
        String(format: "$%d.%02d", cents / 100, cents % 100)
    }

    static func fleetCadenceText(_ figures: HubFleetConsentFigures) -> String {
        HubL10n.format("hub.fleetSetup.consent.cadence",
            fallback: "Hub reads a driving car %1$@, a charging car %2$@, an awake car %3$@ and a car that is updating %4$@. It checks a sleeping car %5$@ and lists your cars %6$@. It polls only while a collection window is open.",
            arguments: [fleetEvery(figures.drivingSeconds), fleetEvery(figures.chargingSeconds),
                        fleetEvery(figures.onlineSeconds), fleetEvery(figures.updatingSeconds),
                        fleetEvery(figures.stateCheckSeconds), fleetEvery(figures.inventorySeconds)])
    }

    static func fleetPriceText(_ figures: HubFleetConsentFigures) -> String {
        let each = String(format: "$%.3f", 1 / Double(max(1, figures.dataRequestsPerDollar)))
        var text = HubL10n.format("hub.fleetSetup.consent.price",
            fallback: "Tesla bills your developer account $1 for every %1$@ vehicle-data requests (%2$@ each).",
            arguments: [String(figures.dataRequestsPerDollar), each])
        if let state = figures.stateRequestsPerDollar, state == figures.dataRequestsPerDollar {
            text += " " + HubL10n.text("hub.fleetSetup.consent.priceState",
                fallback: "A state check or a list of your cars is billed at the same price.")
        } else if let state = figures.stateRequestsPerDollar {
            text += " " + HubL10n.format("hub.fleetSetup.consent.priceStateOther",
                fallback: "A state check or a list of your cars is billed at $1 for every %1$@ requests (%2$@ each).",
                arguments: [String(state), String(format: "$%.3f", 1 / Double(max(1, state)))])
        } else {
            text += " " + HubL10n.text("hub.fleetSetup.consent.priceStateFree",
                fallback: "Tesla does not bill a state check or a list of your cars.")
        }
        return text
    }

    static func fleetCapText(_ figures: HubFleetConsentFigures) -> String {
        var text = HubL10n.format("hub.fleetSetup.consent.cap",
            fallback: "Hub stops polling for the month when its requests reach %1$@.",
            arguments: [fleetDollars(cents: figures.monthlyCapCents)])
        if figures.isCovered(by: .serviceDefaults) {
            text += " " + HubL10n.text("hub.fleetSetup.consent.capDefault",
                fallback: "That is Tesla's $10.00 monthly discount; with these settings it is reached around day 14 of a month.")
        }
        return text
    }

    /// Whether collection is on now, as the service states it.
    static func fleetCollectionText(_ figures: HubFleetConsentFigures) -> String {
        guard figures.collectionIntervalSeconds > 0 else {
            return HubL10n.text("hub.fleetSetup.consent.collectionOff",
                fallback: "Collection is off: Hub polls nothing until you choose Start Collecting.")
        }
        return HubL10n.format("hub.fleetSetup.consent.collectionOn",
            fallback: "Collection is on: Hub's collector runs %1$@.",
            arguments: [fleetEvery(figures.collectionIntervalSeconds)])
    }

    /// The service stated a figure this page has no line for, or none it can use (R2).
    static func fleetUnreadableText(field: String) -> String {
        HubL10n.format("hub.fleetSetup.consent.unreadable",
            fallback: "Hub states a figure this page cannot show (%1$@), so this page cannot state what polling would cost, and nothing can be accepted. Update the app, then open this page again.",
            arguments: [field])
    }

    /// `setup-fleet figures` failed (R2): its own text, and nothing can be accepted.
    static func fleetFiguresFailedText(_ message: String) -> String {
        HubL10n.format("hub.fleetSetup.consent.figuresFailed",
            fallback: "Hub could not state its figures, so this page cannot state what polling would cost, and nothing can be accepted. Hub's answer: %1$@",
            arguments: [message])
    }

    /// The figures in effect changed while the consent page was open (R3): nothing was recorded.
    static var fleetFiguresChangedText: String {
        HubL10n.text("hub.fleetSetup.consent.figuresChanged",
            fallback: "Hub's figures changed while this page was open, so nothing was accepted. Read the figures below and accept them again.")
    }

    /// Set-up itself lists the cars (the redeem and `discover`): Tesla bills those as state checks.
    static var fleetSetUpRequestsText: String {
        HubL10n.text("hub.fleetSetup.consent.setUp",
            fallback: "Setting up makes a few list requests, which Tesla bills at the same price as a state check: signing in lists your cars once, and so does each Find My Car.")
    }

    static var fleetConsentText: String {
        HubL10n.text("hub.fleetSetup.consent.accept",
            fallback: "I accept that Hub polls my car through my Tesla developer application and that Tesla bills these requests to my developer account.")
    }

    fileprivate var fleetPageTitle: String {
        switch fleetStep {
        case .prerequisites: return HubL10n.text("hub.fleetSetup.title.prerequisites", fallback: "Before you set up the Fleet API")
        case .details: return HubL10n.text("hub.fleetSetup.title.details", fallback: "Your Tesla developer application")
        case .publicKey: return HubL10n.text("hub.fleetSetup.title.publicKey", fallback: "Publish Hub's public key")
        case .consent: return HubL10n.text("hub.fleetSetup.title.consent", fallback: "What polling costs")
        case .signIn: return HubL10n.text("hub.fleetSetup.title.signIn", fallback: "Sign in with Tesla")
        case .vehicles: return HubL10n.text("hub.fleetSetup.title.vehicles", fallback: "Find your car")
        case .start: return HubL10n.text("hub.fleetSetup.title.start", fallback: "Start collecting")
        }
    }

    fileprivate var fleetPageSubtitle: String {
        switch fleetStep {
        case .prerequisites: return HubL10n.text("hub.fleetSetup.subtitle.prerequisites", fallback: "The Fleet API needs your own Tesla developer application and a domain you control.")
        case .details: return HubL10n.text("hub.fleetSetup.subtitle.details", fallback: "Enter the details exactly as they are in your Tesla developer application.")
        case .publicKey: return HubL10n.text("hub.fleetSetup.subtitle.publicKey", fallback: "Tesla checks this key on your domain before it registers the domain for your application.")
        case .consent: return HubL10n.text("hub.fleetSetup.subtitle.consent", fallback: "Hub sends nothing that Tesla bills until you accept.")
        case .signIn: return HubL10n.text("hub.fleetSetup.subtitle.signIn", fallback: "Sign in while a collection window is open, and allow vehicle information and location.")
        case .vehicles: return HubL10n.text("hub.fleetSetup.subtitle.vehicles", fallback: "Hub lists the cars on your Tesla account with the stored sign-in.")
        case .start: return HubL10n.text("hub.fleetSetup.subtitle.start", fallback: "Hub polls nothing until you start it.")
        }
    }

    fileprivate var fleetContinueTitle: String {
        if fleetPreparationFailed != nil { return HubL10n.text("hub.fleetSetup.tryAgain", fallback: "Try Again") }
        switch fleetStep {
        case .prerequisites, .details:
            return HubL10n.text("hub.OnboardingWindowController.168.1852", fallback: "Continue")
        case .publicKey:
            return controller.savedFleetApplication?.registered == true
                ? HubL10n.text("hub.OnboardingWindowController.168.1852", fallback: "Continue")
                : HubL10n.text("hub.fleetSetup.register", fallback: "Register Domain")
        case .consent: return HubL10n.text("hub.fleetSetup.acceptCost", fallback: "Accept and Continue")
        case .signIn: return HubL10n.text("hub.OnboardingWindowController.592.1921", fallback: "Sign in with Tesla")
        case .vehicles: return HubL10n.text("hub.fleetSetup.findCar", fallback: "Find My Car")
        case .start: return HubL10n.text("hub.fleetSetup.startCollecting", fallback: "Start Collecting")
        }
    }

    fileprivate var fleetContinueEnabled: Bool {
        if fleetPreparationFailed != nil { return true }
        guard fleetStateUnknown == nil else { return false }
        switch fleetStep {
        case .prerequisites: return fleetChecklist.allSatisfy { $0.state == .on }
        case .details:
            return !fleetClientID.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && !fleetRedirectURI.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && !fleetDomain.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        case .publicKey:
            return controller.savedFleetApplication?.registered == true || !fleetClientSecret.stringValue.isEmpty
        case .consent: return fleetConsent.state == .on && fleetConsentShown != nil && !fleetFiguresReading
        case .signIn: return controller.fleetConsentInEffect && !fleetClientSecret.stringValue.isEmpty
        case .vehicles, .start: return controller.fleetConsentInEffect
        }
    }

    fileprivate var fleetKeyViews: [NSView] {
        if fleetPreparationFailed != nil { return [] }
        switch fleetStep {
        case .prerequisites: return fleetChecklist
        case .details: return [fleetRegion, fleetClientID, fleetRedirectURI, fleetDomain]
        case .publicKey:
            return controller.savedFleetApplication?.registered == true ? [] : [fleetClientSecret]
        case .consent: return fleetConsentShown == nil ? [] : [fleetConsent]
        case .signIn: return [fleetClientSecret]
        case .vehicles, .start: return []
        }
    }

    private static let fleetRegions: [HubFleetRegion] = [.eu, .na, .cn]

    private var selectedFleetRegion: HubFleetRegion {
        Self.fleetRegions[max(0, min(Self.fleetRegions.count - 1, fleetRegion.indexOfSelectedItem))]
    }

    // MARK: Pages

    func fleetBody() -> NSView {
        var views: [NSView] = []
        if let fleetPreparationFailed {
            // Only the failure and its Try Again are offered until the preparation succeeds.
            views.append(noticeView(symbol: "exclamationmark.triangle.fill", tone: .warning, title: nil,
                                    message: fleetPreparationFailed, identifier: "onboarding.fleet.prepare-failed"))
            let stack = NSStackView(views: views)
            stack.orientation = .vertical
            stack.alignment = .leading
            stack.spacing = 14
            stack.identifier = NSUserInterfaceItemIdentifier("onboarding.fleet")
            for view in views { view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
            return stack
        }
        if let fleetStateUnknown {
            views.append(noticeView(symbol: "exclamationmark.triangle.fill", tone: .warning, title: nil,
                                    message: fleetStateUnknown, identifier: "onboarding.fleet.state-unknown"))
        }
        if let fleetNotice {
            views.append(noticeView(symbol: "info.circle.fill", tone: .neutral, title: nil, message: fleetNotice,
                                    identifier: "onboarding.fleet.notice"))
        }
        switch fleetStep {
        case .prerequisites: views.append(fleetPrerequisitesBody())
        case .details: views.append(fleetDetailsBody())
        case .publicKey: views.append(fleetPublicKeyBody())
        case .consent: views.append(fleetConsentBody())
        case .signIn: views.append(fleetSignInBody())
        case .vehicles: views.append(fleetVehiclesBody())
        case .start: views.append(fleetStartBody())
        }
        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        stack.identifier = NSUserInterfaceItemIdentifier("onboarding.fleet.\(fleetStepName)")
        for view in views { view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
        return withError(stack)
    }

    private var fleetStepName: String {
        switch fleetStep {
        case .prerequisites: return "prerequisites"
        case .details: return "details"
        case .publicKey: return "public-key"
        case .consent: return "consent"
        case .signIn: return "sign-in"
        case .vehicles: return "vehicles"
        case .start: return "start"
        }
    }

    private func fleetPrerequisitesBody() -> NSView {
        let items = [
            HubL10n.text("hub.fleetSetup.prerequisite.application", fallback: "A Tesla developer application with the authorization-code grant, the vehicle information and vehicle location scopes, a client ID and secret, and a registered redirect URI."),
            HubL10n.text("hub.fleetSetup.prerequisite.domain", fallback: "A domain you control, where you can publish one file at /.well-known/appspecific/com.tesla.3p.public-key.pem."),
            HubL10n.text("hub.fleetSetup.prerequisite.billing", fallback: "A payment method and a billing limit on your Tesla developer account: Tesla bills the polling.")
        ]
        var rows: [NSView] = []
        for (box, text) in zip(fleetChecklist, items) {
            box.setAccessibilityLabel(text)
            rows.append(wrappingCheckbox(box, title: text))
        }
        let guide = HubActionButton(title: HubL10n.text("hub.fleetSetup.openDeveloper", fallback: "Open Tesla Developer"),
                                    target: self, action: #selector(openFleetGuide))
        configureFlatButton(guide, symbol: "book")
        pageKeyViews.append(guide)
        let stack = NSStackView(views: rows + [guide])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        for row in rows { row.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
        return stack
    }

    private func fleetDetailsBody() -> NSView {
        let form = onboardingForm([
            (HubL10n.text("hub.OnboardingWindowController.region_label", fallback: "Region"), fleetRegion),
            (HubL10n.text("hub.OnboardingWindowController.771.1964", fallback: "Client ID"), fleetClientID),
            (HubL10n.text("hub.fleetSetup.redirectLabel", fallback: "Redirect URI"), fleetRedirectURI),
            (HubL10n.text("hub.fleetSetup.domainLabel", fallback: "Domain"), fleetDomain)
        ])
        let note = featureRow(HubL10n.text("hub.fleetSetup.secretNote", fallback: "Hub asks for the client secret only when it talks to Tesla, and never stores it."), "lock.fill")
        let stack = NSStackView(views: [form, note])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        for view in stack.arrangedSubviews { view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
        return stack
    }

    private func fleetSecretField() -> NSView {
        verticalField(HubL10n.text("hub.fleetSetup.secretLabel", fallback: "Client secret"), fleetClientSecret)
    }

    private func fleetPublicKeyBody() -> NSView {
        let application = controller.savedFleetApplication
        var views: [NSView] = []
        let save = HubActionButton(title: HubL10n.text("hub.fleetSetup.saveKey", fallback: "Save Public Key…"),
                                   target: self, action: #selector(saveFleetPublicKeyPressed))
        save.identifier = NSUserInterfaceItemIdentifier("onboarding.fleet.save-key")
        configureFlatButton(save, symbol: "square.and.arrow.down")
        pageKeyViews.append(save)
        views.append(save)
        let address = application?.publicKeyAddress ?? "/.well-known/appspecific/com.tesla.3p.public-key.pem"
        if let fleetPublicKey {
            views.append(featureRow(HubL10n.format("hub.fleetSetup.keySaved",
                fallback: "Saved to %1$@. Publish this file at %2$@.",
                arguments: [fleetPublicKey.path, address]), "checkmark.circle.fill", color: .systemGreen))
        } else {
            views.append(featureRow(HubL10n.format("hub.fleetSetup.keyPublish",
                fallback: "Save the key, then publish the file at %1$@.", arguments: [address]), "globe"))
        }
        if application?.registered == true {
            views.append(featureRow(HubL10n.format("hub.fleetSetup.registered",
                fallback: "%1$@ is registered with Tesla for this application.",
                arguments: [application?.domain ?? ""]), "checkmark.seal.fill", color: .systemGreen))
        } else {
            views.append(fleetSecretField())
            views.append(featureRow(HubL10n.text("hub.fleetSetup.registerNote", fallback: "Hub sends the client secret to Tesla for this registration only and does not keep it."), "lock.fill"))
        }
        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        for view in views.dropFirst() { view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
        return stack
    }

    private func fleetSignInBody() -> NSView {
        let views: [NSView] = [
            fleetSecretField(),
            featureRow(HubL10n.text("hub.fleetSetup.signInNote", fallback: "Hub sends the client secret to Tesla for this sign-in only and does not keep it. Your Tesla password goes only to Tesla."), "lock.fill")
        ]
        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        for view in views { view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
        return stack
    }

    private func fleetVehiclesBody() -> NSView {
        featureRow(HubL10n.text("hub.fleetSetup.vehiclesNote", fallback: "Finding your car needs an open collection window. It reads the list of cars only: nothing is sent to a car."), "car.side")
    }

    /// The last page: the car Hub found, the figures the owner accepted, and the one button that
    /// runs `setup-fleet activate`.
    private func fleetStartBody() -> NSView {
        var views: [NSView] = []
        let names = controller.snapshot.controlVehicles.map(\.displayName).filter { !$0.isEmpty }
        let car: String
        if !names.isEmpty {
            car = HubL10n.format("hub.fleetSetup.start.cars", fallback: "Hub found %1$@ on your Tesla account.",
                                 arguments: [ListFormatter.localizedString(byJoining: names)])
        } else if let count = controller.lastVehiclesConfigured, count > 0 {
            car = HubL10n.plural("hub.fleetSetup.start.count", count: count,
                                 one: "Hub found 1 car on your Tesla account.", other: "Hub found %d cars on your Tesla account.")
        } else {
            car = HubL10n.text("hub.fleetSetup.start.found", fallback: "Hub found your car on your Tesla account.")
        }
        views.append(featureRow(car, "car.side", color: .systemGreen))
        if let record = controller.fleetConsentRecord {
            let accepted = DateFormatter.localizedString(from: record.acceptedAt, dateStyle: .medium, timeStyle: .short)
            views.append(featureRow(HubL10n.format("hub.fleetSetup.start.accepted",
                fallback: "You accepted these figures on %1$@.", arguments: [accepted]), "checkmark.seal.fill"))
            views += fleetFigureRows(record.figures)
        }
        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        stack.identifier = NSUserInterfaceItemIdentifier("onboarding.fleet.start-page")
        for view in views { view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
        return stack
    }

    /// Asks the service for the figures the consent page shows; once per visit of the page.
    private func loadFleetConsentFigures() {
        guard !fleetFiguresReading else { return }
        fleetFiguresReading = true
        controller.readFleetFigures { [weak self] reading in
            guard let self else { return }
            self.fleetFiguresReading = false
            guard self.state.route == .fleet, self.fleetStep == .consent else { return }
            self.fleetConsentAnswer = reading
            self.render()
        }
    }

    private func fleetFigureRows(_ figures: HubFleetConsentFigures) -> [NSView] {
        let rows: [(String, String, String)] = [
            ("clock", HubL10n.text("hub.fleetSetup.consent.cadenceTitle", fallback: "How often"), Self.fleetCadenceText(figures)),
            ("dollarsign.circle", HubL10n.text("hub.fleetSetup.consent.priceTitle", fallback: "Price"), Self.fleetPriceText(figures)),
            ("gauge", HubL10n.text("hub.fleetSetup.consent.capTitle", fallback: "Monthly cap"), Self.fleetCapText(figures)),
            ("power", HubL10n.text("hub.fleetSetup.consent.collectionTitle", fallback: "Collection"), Self.fleetCollectionText(figures))
        ]
        return rows.map { symbol, title, text in
            let heading = NSTextField(labelWithString: title)
            heading.font = HubTypography.emphasis
            let detail = NSTextField(wrappingLabelWithString: text)
            detail.font = HubTypography.body
            detail.maximumNumberOfLines = 0
            let copy = NSStackView(views: [heading, detail])
            copy.orientation = .vertical
            copy.alignment = .leading
            copy.spacing = 2
            let icon = HubIconTileView(symbol: symbol, accessibilityDescription: nil)
            let row = NSStackView(views: [icon, copy])
            row.alignment = .top
            row.spacing = 12
            detail.widthAnchor.constraint(equalTo: copy.widthAnchor).isActive = true
            return row
        }
    }

    private func fleetConsentBody() -> NSView {
        var views: [NSView] = []
        switch fleetConsentAnswer {
        case nil:
            fleetConsent.state = .off
            fleetConsentShown = nil
            views.append(featureRow(HubL10n.text("hub.fleetSetup.consent.reading",
                fallback: "Hub is stating the figures in effect…"), "hourglass"))
            loadFleetConsentFigures()
        case let .figures(figures)?:
            // A tick given to other figures is not a tick for these.
            if fleetConsentShown != figures { fleetConsent.state = .off }
            fleetConsentShown = figures
            views += fleetFigureRows(figures)
            if figures.stateRequestsPerDollar != nil {
                views.append(featureRow(Self.fleetSetUpRequestsText, "list.bullet"))
            }
            views.append(wrappingCheckbox(fleetConsent, title: Self.fleetConsentText))
        case let .unreadable(field)?:
            // A figure the page cannot show is not stated, and nothing can be accepted.
            fleetConsent.state = .off
            fleetConsentShown = nil
            views.append(noticeView(symbol: "exclamationmark.triangle.fill", tone: .warning, title: nil,
                                    message: Self.fleetUnreadableText(field: field),
                                    identifier: "onboarding.fleet.consent-unreadable"))
        case let .failed(message)?:
            fleetConsent.state = .off
            fleetConsentShown = nil
            views.append(noticeView(symbol: "exclamationmark.triangle.fill", tone: .warning, title: nil,
                                    message: Self.fleetFiguresFailedText(message),
                                    identifier: "onboarding.fleet.consent-failed"))
        }
        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        stack.identifier = NSUserInterfaceItemIdentifier("onboarding.fleet.consent")
        for view in views { view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
        return stack
    }

    // MARK: Actions

    @objc fileprivate func fleetChecklistChanged() { updateFooter() }

    fileprivate func fleetContinue() {
        // A step whose Continue is off offers nothing: no command starts from it.
        guard fleetContinueEnabled else { return }
        if fleetPreparationFailed != nil {
            fleetPreparationFailed = nil
            errorMessage = nil
            render()
            refreshFleetSetupState()
            return
        }
        switch fleetStep {
        case .prerequisites:
            errorMessage = nil
            fleetStep = .details
            fillFleetDetails(from: controller.savedFleetApplication)
            render()
        case .details: fleetSaveDetails()
        case .publicKey:
            if let application = controller.savedFleetApplication, application.registered {
                errorMessage = nil
                fleetStep = fleetStepAfterRegistration()
                render()
            } else {
                fleetRegister()
            }
        case .consent: fleetAcceptConsent()
        case .signIn: fleetSignIn()
        case .vehicles: fleetDiscover()
        case .start: fleetActivate()
        }
    }

    /// After the registration: the consent page, unless the owner's consent is on record.
    private func fleetStepAfterRegistration() -> HubFleetStep {
        fleetStepBehindConsent(.signIn)
    }

    /// `step`, or the consent page first when the owner's consent is not on record: no billed
    /// request (the redeem, `discover`) and no `activate` is offered before it.
    private func fleetStepBehindConsent(_ step: HubFleetStep) -> HubFleetStep {
        fleetResumeAfterConsent = step
        return controller.fleetConsentInEffect ? step : .consent
    }

    /// Accepting the consent page records the time and exactly the answer the page showed;
    /// nothing is sent. The service is asked again first: when the figures in effect differ
    /// from the shown ones, nothing is recorded, and the page shows the new figures, clears the
    /// tick and asks again (R3).
    private func fleetAcceptConsent() {
        guard fleetConsent.state == .on, let shown = fleetConsentShown, !fleetFiguresReading else { return }
        fleetFiguresReading = true
        setBusy(true, message: HubL10n.text("hub.fleetSetup.busy.figures", fallback: "Checking Hub's figures…"),
                operation: .fleet)
        controller.readFleetFigures { [weak self] reading in
            guard let self else { return }
            self.fleetFiguresReading = false
            self.setBusy(false)
            guard self.state.route == .fleet, self.fleetStep == .consent else { return }
            self.fleetConsentAnswer = reading
            guard case let .figures(current) = reading, current == shown else {
                HubAppLog.shared.record("fleet_setup.consent_figures_changed", category: "account")
                self.fleetConsent.state = .off
                self.errorMessage = nil
                if case .figures = reading { self.fleetNotice = Self.fleetFiguresChangedText } else { self.fleetNotice = nil }
                self.render()
                return
            }
            self.recordFleetConsent(shown)
        }
    }

    private func recordFleetConsent(_ shown: HubFleetConsentFigures) {
        do { try controller.recordFleetConsent(shown) } catch {
            showInlineError(error.localizedDescription)
            return
        }
        HubAppLog.shared.record("fleet_setup.consent_accepted", category: "account")
        fleetConsent.state = .off
        fleetConsentShown = nil
        errorMessage = nil
        fleetNotice = nil
        fleetStep = fleetResumeAfterConsent
        render()
    }

    private func fillFleetDetails(from application: HubFleetApplication?) {
        guard let application else { return }
        if fleetClientID.stringValue.isEmpty { fleetClientID.stringValue = application.clientID }
        if fleetRedirectURI.stringValue.isEmpty { fleetRedirectURI.stringValue = application.redirectURI }
        if fleetDomain.stringValue.isEmpty { fleetDomain.stringValue = application.domain }
        if let index = Self.fleetRegions.firstIndex(of: application.region) { fleetRegion.selectItem(at: index) }
    }

    /// Section 10, steps 1 and 2: the details are kept (never the secret), then the Hub becomes a
    /// Fleet Hub with collection off and runs.
    private func fleetSaveDetails() {
        var application: HubFleetApplication
        do {
            application = try HubFleetApplication.validated(clientID: fleetClientID.stringValue,
                                                            region: selectedFleetRegion,
                                                            redirectURI: fleetRedirectURI.stringValue,
                                                            domain: fleetDomain.stringValue)
        } catch {
            showInlineError(error.localizedDescription)
            return
        }
        if let saved = controller.savedFleetApplication, saved.sameRegistration(as: application) {
            application.registered = saved.registered
        }
        fleetClientSecret.stringValue = ""
        setBusy(true, message: HubL10n.text("hub.fleetSetup.busy.prepare", fallback: "Preparing Hub for the Fleet API…"),
                operation: .fleet)
        controller.prepareFleetHub { [weak self] result in
            guard let self else { return }
            self.setBusy(false)
            switch result {
            case let .success(preparation):
                do { try self.controller.saveFleetApplication(application) } catch {
                    self.showInlineError(error.localizedDescription)
                    return
                }
                let next = { [weak self] in
                    guard let self else { return }
                    self.errorMessage = nil
                    self.fleetNotice = nil
                    self.fleetStep = application.registered ? self.fleetStepAfterRegistration() : .publicKey
                    self.render()
                }
                if preparation == .serviceStopped { self.fleetStartService(then: next) } else { next() }
            case let .failure(error):
                self.fleetFailed(error)
            }
        }
    }

    @objc fileprivate func saveFleetPublicKeyPressed() {
        guard let window, !interactionBlocked, !filePanelOpen else { return }
        let panel = NSSavePanel()
        panel.title = HubL10n.text("hub.fleetSetup.saveKeyTitle", fallback: "Save Public Key")
        panel.nameFieldStringValue = "com.tesla.3p.public-key.pem"
        panel.canCreateDirectories = true
        filePanelOpen = true
        presentFleetKeySavePanel(panel, window) { [weak self] url in
            guard let self else { return }
            self.filePanelOpen = false
            guard let url else { return }
            self.fleetSavePublicKey(to: url, afterStart: false)
        }
    }

    private func fleetSavePublicKey(to url: URL, afterStart: Bool) {
        fleetClientSecret.stringValue = ""
        setBusy(true, message: HubL10n.text("hub.fleetSetup.busy.key", fallback: "Saving the public key…"),
                operation: .fleet)
        controller.saveFleetPublicKey(to: url) { [weak self] result in
            guard let self else { return }
            self.setBusy(false)
            switch result {
            case let .success(key):
                self.fleetPublicKey = key
                self.errorMessage = nil
                self.render()
            case let .failure(error):
                self.fleetFailed(error, retry: afterStart ? nil : { [weak self] in
                    self?.fleetSavePublicKey(to: url, afterStart: true)
                })
            }
        }
    }

    private func fleetRegister() {
        // The registration owns the secret for its own call: taken from the field, which is
        // cleared as the command starts, into a buffer the call wipes.
        guard let secret = takeFleetSecret() else { return }
        guard let application = controller.savedFleetApplication else { secret.wipe(); return fleetDetailsMissing() }
        setBusy(true, message: HubL10n.text("hub.fleetSetup.busy.register", fallback: "Registering your domain with Tesla…"),
                operation: .fleet)
        controller.registerFleetDomain(application, clientSecret: secret) { [weak self] result in
            secret.wipe()
            guard let self else { return }
            self.setBusy(false)
            switch result {
            case .success:
                self.errorMessage = nil
                self.fleetStep = self.fleetStepAfterRegistration()
                self.render()
            case let .failure(error):
                self.fleetFailed(error)
            }
        }
    }

    /// Takes the typed client secret into a wiping buffer and clears the field, as every command
    /// of the route starts. A secret that cannot be passed is wiped and refused: the owner types
    /// it again.
    private func takeFleetSecret() -> HubFleetSecret? {
        let secret = HubFleetSecret(fleetClientSecret.stringValue)
        fleetClientSecret.stringValue = ""
        #if DEBUG
        fleetSecretsForTests.append(secret)
        #endif
        guard secret.isPassable else {
            secret.wipe()
            showInlineError(HubL10n.text("hub.fleetSetup.secretUnpassable",
                fallback: "Enter the client secret of your Tesla developer application. A secret with a quote, a backslash or a control character cannot be used."))
            return nil
        }
        return secret
    }

    /// Ends the sign-in attempt waiting for the Tesla window, if any: its secret is wiped and
    /// its window closed, and a late callback of that attempt is ignored. Every path that ends
    /// an attempt comes here: success, a refused or invalid begin, a window that could not be
    /// built, a wrong `state`, access denied, a navigation failure, cancel, a closed window,
    /// Back, a newer attempt and quit.
    private func endFleetSignIn() {
        fleetClientSecret.stringValue = ""
        guard let secret = fleetSignInSecret else { return }
        fleetSignInSecret = nil
        secret.wipe()
        if let auth = authWindow {
            authWindow = nil
            auth.close()
            updateWindowCloseAvailability()
            updateFooter()
        }
    }

    /// Section 10, step 4: begin, the browser leg on the registered redirect URI with its `state`
    /// checked, then the redeem with the attempt's binding. The attempt owns the secret from
    /// `begin` to the end of its redeem.
    private func fleetSignIn() {
        guard authWindow == nil else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        // The typed secret is taken first (the field is cleared), then a newer attempt ends the
        // one before it.
        let taken = takeFleetSecret()
        endFleetSignIn()
        guard let secret = taken else { return }
        guard let application = controller.savedFleetApplication else { secret.wipe(); return fleetDetailsMissing() }
        fleetSignInSecret = secret
        setBusy(true, message: HubL10n.text("hub.fleetSetup.busy.begin", fallback: "Preparing the Tesla sign-in…"),
                operation: .fleet)
        controller.beginFleetSignIn(application) { [weak self] result in
            guard let self else { secret.wipe(); return }
            self.setBusy(false)
            guard self.fleetSignInSecret === secret else { secret.wipe(); return }
            switch result {
            case let .success(attempt):
                self.openFleetLogin(attempt, secret: secret)
            case let .failure(error):
                self.endFleetSignIn()
                self.fleetFailed(error)
            }
        }
    }

    private func openFleetLogin(_ attempt: HubFleetSignInAttempt, secret: HubFleetSecret) {
        HubAppLog.shared.record("authentication.started", category: "account", fields: ["provider": "fleet"])
        let finished: (Result<TeslaAuthCodeGrant, Error>) -> Void = { [weak self] result in
            guard let self else { secret.wipe(); return }
            // A callback of an attempt that has ended (Back, close, quit, a newer attempt) is ignored.
            guard self.fleetSignInSecret === secret else { secret.wipe(); return }
            self.authWindow = nil
            self.updateWindowCloseAvailability()
            self.updateFooter()
            switch result {
            case let .success(grant):
                HubAppLog.shared.record("authentication.completed", category: "account", fields: ["provider": "fleet"])
                self.fleetRedeem(attempt, code: grant.code, secret: secret)
            case let .failure(error):
                self.endFleetSignIn()
                if error as? TeslaAuthError == .cancelled {
                    HubAppLog.shared.record("authentication.cancelled", category: "account", fields: ["provider": "fleet"])
                    return
                }
                HubAppLog.shared.record("authentication.failed", category: "account", level: "WARN",
                                        fields: ["provider": "fleet", "error_code": HubAppLog.errorCode(error)])
                self.fleetFailed(error)
            }
        }
        do {
            if let presentFleetSignIn {
                try presentFleetSignIn(attempt.flow, finished)
                return
            }
            let auth = try TeslaAuthWindowController(flow: attempt.flow, completion: finished)
            authWindow = auth
            updateWindowCloseAvailability()
            updateFooter()
            auth.showWindow(nil)
            auth.window?.makeKeyAndOrderFront(nil)
        } catch {
            endFleetSignIn()
            fleetFailed(error)
        }
    }

    private func fleetRedeem(_ attempt: HubFleetSignInAttempt, code: String, secret: HubFleetSecret) {
        fleetClientSecret.stringValue = ""
        setBusy(true, message: HubL10n.text("hub.fleetSetup.busy.redeem", fallback: "Signing in with Tesla…"),
                operation: .fleet)
        controller.redeemFleetSignIn(attempt, code: code, clientSecret: secret) { [weak self] result in
            secret.wipe()
            guard let self else { return }
            self.setBusy(false)
            if self.fleetSignInSecret === secret { self.endFleetSignIn() }
            switch result {
            case let .success(.vehiclesConfigured(count)) where count > 0:
                self.errorMessage = nil
                self.fleetNotice = nil
                self.fleetStep = .start
                self.refreshFleetCars()
            case .success(.vehiclesConfigured):
                self.errorMessage = nil
                self.fleetNotice = HubFleetRefusal.message(code: "discovery_no_vehicles")
                self.fleetStep = .vehicles
                self.render()
            case let .success(.discoveryFailed(_, message)):
                // Signed in: the next step is finding the car, not a new sign-in.
                self.errorMessage = nil
                self.fleetNotice = message
                self.fleetStep = .vehicles
                self.render()
            case let .failure(error):
                self.fleetFailed(error)
            }
        }
    }

    private func fleetDiscover(afterStart: Bool = false) {
        fleetClientSecret.stringValue = ""
        setBusy(true, message: HubL10n.text("hub.fleetSetup.busy.discover", fallback: "Finding your car…"), operation: .fleet)
        controller.discoverFleetVehicles { [weak self] result in
            guard let self else { return }
            self.setBusy(false)
            switch result {
            case let .success(count) where count > 0:
                self.errorMessage = nil
                self.fleetNotice = nil
                self.fleetStep = .start
                self.refreshFleetCars()
            case .success:
                self.showInlineError(HubFleetRefusal.message(code: "discovery_no_vehicles") ?? "")
            case let .failure(error):
                self.fleetFailed(error, retry: afterStart ? nil : { [weak self] in self?.fleetDiscover(afterStart: true) })
            }
        }
    }

    /// Shows the last page, then reads the Hub's status (local, nothing sent to Tesla) so the
    /// page names the car it found.
    private func refreshFleetCars() {
        render()
        controller.refresh { [weak self] _ in
            DispatchQueue.main.async {
                guard let self, self.state.route == .fleet, self.fleetStep == .start, !self.busy else { return }
                self.render()
            }
        }
    }

    /// Section 10, step 6: the last page runs `setup-fleet activate`, only with the owner's
    /// consent on record.
    private func fleetActivate(afterStart: Bool = false) {
        fleetClientSecret.stringValue = ""
        guard controller.fleetConsentInEffect else {
            fleetStep = fleetStepBehindConsent(.start)
            render()
            return
        }
        setBusy(true, message: HubL10n.text("hub.fleetSetup.busy.activate", fallback: "Starting collection…"), operation: .fleet)
        controller.activateFleetCollection { [weak self] result in
            guard let self else { return }
            switch result {
            case .success:
                self.fleetNotice = nil
                self.setupFinished(.success(()))
            case let .failure(error):
                self.setBusy(false)
                self.fleetFailed(error, retry: afterStart ? nil : { [weak self] in self?.fleetActivate(afterStart: true) })
            }
        }
    }

    private func fleetDetailsMissing() {
        fleetStep = .details
        showInlineError(HubL10n.text("hub.fleetSetup.detailsMissing",
            fallback: "Enter your Tesla developer application's details again to continue."))
    }

    /// Shows a refusal. When the outcome is unknown (the answer was lost), the state is read
    /// again and the page moves to the step it names.
    /// `retry` repeats a step that carries no secret once Hub is running again; a step that
    /// carried the client secret asks for it again instead.
    private func fleetFailed(_ error: Error, readsState: Bool = true, retry: (() -> Void)? = nil) {
        let underlying = HubActionError.settingQuiescenceAside(error)
        if (underlying as? HubFleetRefusal)?.code == "service_not_running" {
            fleetStartService { [weak self] in
                guard let self else { return }
                if let retry { return retry() }
                self.errorMessage = nil
                self.fleetNotice = HubL10n.text("hub.fleetSetup.serviceStarted",
                    fallback: "Hub was not running, so Hub started it. Continue from this step.")
                self.render()
            }
            return
        }
        showInlineError(error.localizedDescription)
        if readsState, (underlying as? HubFleetRefusal)?.outcomeUnknown == true { refreshFleetSetupState() }
    }

    /// Hub's service is not running (a step refused with `service_not_running`, or the
    /// preparation found the installed service stopped): the route starts it through the app's
    /// start action and continues with `next`. A failed start is shown with the service's text.
    private func fleetStartService(then next: @escaping () -> Void) {
        HubAppLog.shared.record("fleet_setup.service_start_requested", category: "account")
        setBusy(true, message: HubL10n.text("hub.fleetSetup.busy.start", fallback: "Starting Hub…"), operation: .fleet)
        controller.startHub { [weak self] result in
            let deliver = {
                guard let self else { return }
                self.setBusy(false)
                switch result {
                case .success: next()
                case let .failure(error): self.showInlineError(error.localizedDescription)
                }
            }
            if Thread.isMainThread { deliver() } else { DispatchQueue.main.async(execute: deliver) }
        }
    }

    // MARK: Waiting states

    /// On every entry to the Fleet route and every return: first makes sure a Fleet Hub without
    /// the owner's consent on record cannot start collecting (`prepareFleetHub` sets the interval
    /// to 0 unless the record matches the figures in effect), then reads which Fleet state the
    /// Hub is in (`preflight`) and shows that step with its next action, the consent page first
    /// where consent is missing. So a set-up continues from where it stopped, also after a quit.
    func refreshFleetSetupState() {
        guard state.route == .fleet, !controller.previewMode, controller.hubConfiguredForFleet else {
            fillFleetDetails(from: controller.savedFleetApplication)
            return
        }
        fleetStateReading &+= 1
        let reading = fleetStateReading
        setBusy(true, message: HubL10n.text("hub.fleetSetup.busy.prepare", fallback: "Preparing Hub for the Fleet API…"),
                operation: .fleet)
        controller.prepareFleetHub { [weak self] prepared in
            guard let self, reading == self.fleetStateReading, self.state.route == .fleet else { return }
            self.setBusy(false)
            switch prepared {
            case let .failure(error):
                guard HubActionError.settingQuiescenceAside(error) is HubFleetPreparationLeftStopped else {
                    self.fleetFailed(error, readsState: false)
                    return
                }
                // R1: Hub was left stopped; nothing of the route is offered but trying again.
                self.errorMessage = nil
                self.fleetPreparationFailed = error.localizedDescription
                self.render()
                HubAccessibility.announce("Setup failed: \(error.localizedDescription)",
                                          from: self.onboardingContainer.map { $0 as Any } ?? self)
            case .success(.serviceStopped):
                self.fleetStartService { [weak self] in self?.readFleetSetupState(reading) }
            case .success(.ready):
                self.readFleetSetupState(reading)
            }
        }
    }

    private func readFleetSetupState(_ reading: UInt64) {
        controller.readFleetSetupState { [weak self] result in
            guard let self, reading == self.fleetStateReading, self.state.route == .fleet,
                  !self.busy, self.authWindow == nil else { return }
            switch result {
            case let .success(setupState): self.applyFleetSetupState(setupState)
            case let .failure(error): self.showInlineError(error.localizedDescription)
            }
        }
    }

    private func applyFleetSetupState(_ setupState: HubFleetSetupState) {
        let saved = controller.savedFleetApplication
        fillFleetDetails(from: saved)
        fleetStateUnknown = nil
        switch setupState {
        case .awaitingSignIn:
            fleetNotice = HubL10n.text("hub.fleetSetup.state.awaitingSignIn",
                fallback: "Hub is waiting for your Tesla sign-in. Sign in with Tesla to continue.")
            // An application saved but not yet registered continues at the public key and the
            // registration, not at the sign-in.
            if let saved {
                fleetStep = saved.registered ? fleetStepBehindConsent(.signIn) : .publicKey
            } else {
                fleetStep = .details
            }
        case .awaitingVehicles:
            fleetNotice = HubL10n.text("hub.fleetSetup.state.awaitingVehicles",
                fallback: "You are signed in, and Hub has not found your car yet. Find your car to continue.")
            fleetStep = fleetStepBehindConsent(.vehicles)
        case .awaitingActivation:
            fleetNotice = HubL10n.text("hub.fleetSetup.state.awaitingActivation",
                fallback: "You are signed in and your car is found. Hub polls nothing until you start collecting.")
            fleetStep = fleetStepBehindConsent(.start)
        case .collecting:
            fleetNotice = HubL10n.text("hub.fleetSetup.state.collecting",
                fallback: "Hub is collecting through the Fleet API. Sign in again only to change the Tesla account.")
            fleetStep = saved == nil ? .details : fleetStepBehindConsent(.signIn)
        case .idle:
            fleetNotice = nil
            if let saved { fleetStep = saved.registered ? fleetStepBehindConsent(.signIn) : .publicKey }
        case let .unknown(state, serviceText):
            // Shown as it is: no next step is chosen from a state this app does not know.
            fleetNotice = nil
            fleetStateUnknown = HubL10n.format("hub.fleetSetup.state.unknown",
                fallback: "Hub reports a set-up state this version of the app does not know (%1$@), so it offers no next step. Hub's answer: %2$@",
                arguments: [state, serviceText])
        case .notFleet:
            return
        }
        render()
    }

    #if DEBUG
    var fleetStepForTests: HubFleetStep { fleetStep }
    var fleetNoticeForTests: String? { fleetNotice }
    var fleetStateUnknownForTests: String? { fleetStateUnknown }
    var fleetPreparationFailedForTests: String? { fleetPreparationFailed }
    var fleetConsentAnswerForTests: HubFleetConsentReading? { fleetConsentAnswer }
    var errorMessageForTests: String? { errorMessage }
    var fleetSecretForTests: String { fleetClientSecret.stringValue }
    var fleetSignInPendingForTests: Bool { fleetSignInSecret != nil }
    var fleetPublicKeyForTests: HubFleetPublicKey? { fleetPublicKey }
    var fleetConsentEnabledForTests: Bool { continueButton.isEnabled }
    func enterFleetSecretForTests(_ secret: String) {
        fleetClientSecret.stringValue = secret
        controlTextDidChange(Notification(name: NSControl.textDidChangeNotification, object: fleetClientSecret))
    }
    func enterFleetDetailsForTests(clientID: String, region: HubFleetRegion, redirectURI: String, domain: String) {
        fleetClientID.stringValue = clientID
        fleetRedirectURI.stringValue = redirectURI
        fleetDomain.stringValue = domain
        if let index = Self.fleetRegions.firstIndex(of: region) { fleetRegion.selectItem(at: index) }
        updateFooter()
    }
    func tickFleetPrerequisitesForTests() {
        fleetChecklist.forEach { $0.state = .on }
        updateFooter()
    }
    func acceptFleetConsentForTests() { fleetConsent.state = .on; updateFooter() }
    func saveFleetPublicKeyForTests() { saveFleetPublicKeyPressed() }
    func backForTests() { backPressed() }
    #endif
}
