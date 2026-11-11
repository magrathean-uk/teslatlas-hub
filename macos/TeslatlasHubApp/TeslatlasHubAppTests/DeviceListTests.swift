// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

/// M06-T04: paired devices with their car grants, and Revoke through the existing
/// `control revoke-device` route after a confirmation. No window is shown.
final class DeviceListTests: XCTestCase {
    private let phone = UUID(uuidString: "11111111-2222-4333-8444-555555555555")!
    private let tablet = UUID(uuidString: "66666666-7777-4888-8999-aaaaaaaaaaaa")!
    private let revokedDevice = UUID(uuidString: "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff")!
    private let modelY = UUID(uuidString: "0a0a0a0a-0b0b-4c0c-8d0d-0e0e0e0e0e0e")!
    private let model3 = UUID(uuidString: "1a1a1a1a-1b1b-4c1c-8d1d-1e1e1e1e1e1e")!

    private func labels(_ view: NSView) -> [String] {
        ((view as? NSTextField).map { [$0.stringValue] } ?? []) + view.subviews.flatMap(labels)
    }

    private func buttons(_ view: NSView) -> [NSButton] {
        ((view as? NSButton).map { [$0] } ?? []) + view.subviews.flatMap(buttons)
    }

    private func listing() -> String {
        let future = Int64(Date().addingTimeInterval(86_400 * 30).timeIntervalSince1970 * 1000)
        return """
        [{"device_id":"\(phone.uuidString.lowercased())","display_name":"Owner iPhone","created_at_ms":1700000000000,\
        "expires_at_ms":\(future),"revoked_at_ms":null,"last_authenticated_at_ms":1700000500000,\
        "vehicles":["\(modelY.uuidString.lowercased())","\(model3.uuidString.lowercased())"]},\
        {"device_id":"\(tablet.uuidString.lowercased())","display_name":"Kitchen tablet","created_at_ms":1700000100000,\
        "expires_at_ms":\(future),"revoked_at_ms":null,"last_authenticated_at_ms":null,\
        "vehicles":["\(model3.uuidString.lowercased())"]},\
        {"device_id":"\(revokedDevice.uuidString.lowercased())","display_name":"Old phone","created_at_ms":1600000000000,\
        "expires_at_ms":\(future),"revoked_at_ms":1650000000000,"last_authenticated_at_ms":null,"vehicles":[]}]
        """
    }

    private func makeView(_ backend: FakeMaintenanceBackend,
                          mutationAllowed: @escaping () -> Bool = { true },
                          answer: NSApplication.ModalResponse = .alertSecondButtonReturn,
                          alerts: ((NSAlert) -> Void)? = nil) -> DeviceListView {
        DeviceListView(backend: backend, mutationAllowed: mutationAllowed,
                       confirmationPresenter: { alert, _ in alerts?(alert); return answer },
                       date: { "T\(Int($0.timeIntervalSince1970))" })
    }

    func testTheListingIsReadWithItsCarGrants() throws {
        let devices = try XCTUnwrap(HubPairedDevice.list(from: listing()))
        XCTAssertEqual(devices.map(\.name), ["Owner iPhone", "Kitchen tablet", "Old phone"])
        XCTAssertEqual(devices[0].vehicles, [modelY, model3])
        XCTAssertTrue(devices[0].isActive)
        XCTAssertFalse(devices[2].isActive)
        let names = [modelY: "Model Y", model3: "Model 3"]
        XCTAssertEqual(devices[0].grantText(vehicleNames: names), "Model Y, Model 3")
        XCTAssertEqual(devices[1].grantText(vehicleNames: [:]), "Car 1a1a1a1a")
        XCTAssertEqual(devices[2].grantText(vehicleNames: names), "No cars")
        XCTAssertEqual(devices[0].stateText(date: { "T\(Int($0.timeIntervalSince1970))" }), "Last seen T1700000500")
        XCTAssertEqual(devices[1].stateText(), "Not seen yet")
        XCTAssertEqual(devices[2].stateText(date: { "T\(Int($0.timeIntervalSince1970))" }), "Revoked T1650000000")
        XCTAssertNil(HubPairedDevice.list(from: "not json"))
        XCTAssertEqual(HubPairedDevice.list(from: "[]"), [])
        XCTAssertFalse(listing().lowercased().contains("token"), "the listing the app reads holds no bearer")
    }

    func testDevicesAreShownWithTheirCarsAndOnlyActiveOnesOfferRevoke() throws {
        let backend = FakeMaintenanceBackend()
        backend.vehicleNames = [modelY: "Model Y", model3: "Model 3"]
        backend.devicesAnswer = .success(try XCTUnwrap(HubPairedDevice.list(from: listing())))
        let view = makeView(backend)
        view.reload()
        let shown = labels(view.rowsStack)
        XCTAssertTrue(shown.contains("Owner iPhone"))
        XCTAssertTrue(shown.contains("Cars: Model Y, Model 3"))
        XCTAssertTrue(shown.contains("Cars: Model 3"))
        XCTAssertTrue(shown.contains("Old phone"))
        XCTAssertTrue(shown.contains("Revoked T1650000000"))
        let revokes = buttons(view).filter { $0.identifier?.rawValue.hasPrefix("hub.devices.revoke.") == true }
        XCTAssertEqual(Set(revokes.compactMap { $0.identifier?.rawValue }),
                       [DeviceListView.revokeIdentifier(phone), DeviceListView.revokeIdentifier(tablet)])
        XCTAssertTrue(view.messageLabel.isHidden)
    }

    func testRevokeAsksFirstAndUsesTheExistingRoute() throws {
        let backend = FakeMaintenanceBackend()
        backend.devicesAnswer = .success(try XCTUnwrap(HubPairedDevice.list(from: listing())))
        var shownAlerts: [NSAlert] = []
        var answer = NSApplication.ModalResponse.alertFirstButtonReturn
        let view = DeviceListView(backend: backend,
                                  confirmationPresenter: { alert, _ in shownAlerts.append(alert); return answer },
                                  date: { _ in "T" })
        view.reload()
        func revokeButton() throws -> NSButton {
            try XCTUnwrap(buttons(view).first { $0.identifier?.rawValue == DeviceListView.revokeIdentifier(phone) })
        }
        // Cancel (the default) revokes nothing.
        try revokeButton().performClick(nil)
        XCTAssertEqual(shownAlerts.count, 1)
        XCTAssertEqual(shownAlerts[0].messageText, "Revoke Owner iPhone?")
        XCTAssertEqual(shownAlerts[0].buttons.map(\.title), ["Cancel", "Revoke"])
        XCTAssertEqual(shownAlerts[0].buttons[0].keyEquivalent, "\r", "Cancel answers Return")
        XCTAssertEqual(backend.revoked, [])

        answer = .alertSecondButtonReturn
        try revokeButton().performClick(nil)
        XCTAssertEqual(backend.revoked, [phone])
        XCTAssertFalse(try revokeButton().isEnabled, "no second revoke while one runs")
        let listCalls = backend.deviceListCalls
        var afterRevoke = try XCTUnwrap(HubPairedDevice.list(from: listing()))
        afterRevoke.removeFirst()
        backend.devicesAnswer = .success(afterRevoke)
        try XCTUnwrap(backend.revokeCompletion)(.success(()))
        XCTAssertEqual(backend.deviceListCalls, listCalls + 1, "the list is read again")
        XCTAssertFalse(labels(view.rowsStack).contains("Owner iPhone"))
        XCTAssertEqual(view.messageLabel.stringValue, "Owner iPhone was revoked.")
    }

    func testTheControllerRevokesThroughControlRevokeDevice() throws {
        let home = HubTestScratch.fixtureRoot("DeviceList")
        defer { try? FileManager.default.removeItem(at: home) }
        let runner = RecordingMaintenanceRunner()
        let devicesListing = listing()
        runner.answer = { arguments in
            arguments.contains("paired-devices")
                ? .success(devicesListing)
                : .success(#"{"status":"revoked","deviceId":"x"}"#)
        }
        let controller = HubController(environment: [:], commandRunner: runner, launchAgent: nil,
                                       dataRemover: nil, homeDirectory: home, serviceInstalledOverride: true)
        let backend = HubControllerMaintenance(controller: controller)
        let listed = expectation(description: "listed")
        backend.listPairedDevices { result in
            XCTAssertEqual((try? result.get())?.count, 3)
            listed.fulfill()
        }
        let revoked = expectation(description: "revoked")
        backend.revokeDevice(phone) { result in
            if case let .failure(error) = result { XCTFail("revoke failed: \(error)") }
            revoked.fulfill()
        }
        wait(for: [listed, revoked], timeout: 2)
        XCTAssertEqual(runner.arguments.map { Array($0.dropFirst(2)) },
                       [["control", "paired-devices"], ["control", "revoke-device", phone.uuidString.lowercased()]])
    }

    func testFailuresAndAClosedOwnerGateAreHandled() throws {
        let backend = FakeMaintenanceBackend()
        backend.devicesAnswer = .success(try XCTUnwrap(HubPairedDevice.list(from: listing())))
        var asked = 0
        let gated = makeView(backend, mutationAllowed: { false }, alerts: { _ in asked += 1 })
        gated.reload()
        try XCTUnwrap(buttons(gated).first { $0.identifier?.rawValue == DeviceListView.revokeIdentifier(phone) })
            .performClick(nil)
        XCTAssertEqual(asked, 0)
        XCTAssertEqual(backend.revoked, [])
        gated.setMutationsEnabled(false)
        XCTAssertTrue(buttons(gated).filter { $0.identifier?.rawValue.hasPrefix("hub.devices.revoke.") == true }
            .allSatisfy { !$0.isEnabled })

        let view = makeView(backend)
        view.reload()
        try XCTUnwrap(buttons(view).first { $0.identifier?.rawValue == DeviceListView.revokeIdentifier(tablet) })
            .performClick(nil)
        try XCTUnwrap(backend.revokeCompletion)(.failure(HubActionError.commandExited(1, "Error: device not found")))
        XCTAssertFalse(view.messageLabel.isHidden)
        XCTAssertEqual(view.messageLabel.stringValue, "Error: device not found")
        XCTAssertTrue(try XCTUnwrap(buttons(view).first {
            $0.identifier?.rawValue == DeviceListView.revokeIdentifier(tablet)
        }).isEnabled)

        backend.devicesAnswer = .success([])
        view.reload()
        XCTAssertEqual(view.messageLabel.stringValue, "No paired devices.")
        backend.devicesAnswer = .failure(HubActionError.commandExited(1, "Error: service_not_running: the Hub is not running"))
        view.reload()
        XCTAssertEqual(view.messageLabel.stringValue, "the Hub is not running")
    }

    func testServiceDetailsShowBackupsAndDevicesOnlyWithABackendAndOnceStatusIsKnown() throws {
        let backend = FakeMaintenanceBackend()
        backend.devicesAnswer = .success(try XCTUnwrap(HubPairedDevice.list(from: listing())))
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        var checking = HubSnapshot.previewRunning
        checking.checkingStatus = true
        let details = ServiceDetailsWindowController(snapshot: checking, controller: controller,
                                                     lifecycleActions: .init(start: {}, stop: {}, restart: {}),
                                                     maintenance: backend)
        defer { details.window?.close() }
        XCTAssertNotNil(details.backupSettings)
        XCTAssertNotNil(details.deviceList)
        details.reloadMaintenance()
        XCTAssertEqual(backend.deviceListCalls, 0, "nothing runs beside a pending status read")
        XCTAssertEqual(backend.statusCalls, 0)
        details.update(snapshot: .previewRunning)
        XCTAssertEqual(backend.deviceListCalls, 1)
        XCTAssertEqual(backend.statusCalls, 1)
        details.update(snapshot: .previewRunning)
        XCTAssertEqual(backend.deviceListCalls, 1, "a later status refresh does not reload")
        let root = try XCTUnwrap(details.window?.contentView)
        XCTAssertTrue(labels(root).contains("Backups"))
        XCTAssertTrue(labels(root).contains("Paired devices"))
        details.setMutationsEnabled(false)
        XCTAssertFalse(try XCTUnwrap(details.backupSettings).backUpNowButton.isEnabled)

        let plain = ServiceDetailsWindowController(snapshot: .previewRunning, controller: controller)
        defer { plain.window?.close() }
        XCTAssertNil(plain.backupSettings)
        XCTAssertNil(plain.deviceList)
        XCTAssertFalse(labels(try XCTUnwrap(plain.window?.contentView)).contains("Paired devices"))
    }
}
