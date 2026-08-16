import XCTest
@testable import Teslatlas_Hub

final class HubControllerTests: XCTestCase {
    func testMainWindowBuildsInPreviewMode() {
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
        let windowController = MainWindowController(controller: controller)
        XCTAssertEqual(windowController.window?.title, "Teslatlas Hub")
        XCTAssertEqual(windowController.window?.contentRect(forFrameRect: windowController.window!.frame).size,
                       NSSize(width: 900, height: 568))
    }

    func testPreviewIsReadOnly() {
        let runner = CountingRunner()
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"], commandRunner: runner)
        XCTAssertTrue(controller.previewMode)
        XCTAssertEqual(controller.snapshot.health, .running)

        let expectation = expectation(description: "actions rejected")
        var failures = 0
        let done: (Result<Void, Error>) -> Void = { result in
            if case .failure = result { failures += 1 }
            if failures == 4 { expectation.fulfill() }
        }
        controller.installService(completion: done)
        controller.importTeslaMate(source: "postgres://example", carID: "1", passwordFile: "/tmp/password", encryptionKeyFile: "/tmp/encryption", completion: done)
        controller.stopHub(completion: done)
        controller.restartHub(completion: done)
        wait(for: [expectation], timeout: 1)
        XCTAssertEqual(runner.calls, 0)
    }

    func testServicePlanBootstrapsOnlyWhenUnloaded() {
        let loaded = LaunchctlServiceController.commandPlan(action: .restart, loaded: true, domain: "gui/1", service: "gui/1/com.teslatlas.hub", plist: "/tmp/hub.plist")
        XCTAssertEqual(loaded, [["kickstart", "-k", "gui/1/com.teslatlas.hub"]])
        let unloaded = LaunchctlServiceController.commandPlan(action: .restart, loaded: false, domain: "gui/1", service: "gui/1/com.teslatlas.hub", plist: "/tmp/hub.plist")
        XCTAssertEqual(unloaded, [["bootstrap", "gui/1", "/tmp/hub.plist"], ["kickstart", "-k", "gui/1/com.teslatlas.hub"]])
        XCTAssertEqual(LaunchctlServiceController.commandPlan(action: .stop, loaded: true, domain: "gui/1", service: "gui/1/com.teslatlas.hub", plist: "/tmp/hub.plist"), [["bootout", "gui/1/com.teslatlas.hub"]])
        XCTAssertEqual(LaunchctlServiceController.commandPlan(action: .stop, loaded: false, domain: "gui/1", service: "gui/1/com.teslatlas.hub", plist: "/tmp/hub.plist"), [])
    }

    func testCommandRunnerPreservesMigrationPromptInput() {
        let runner = CountingRunner()
        runner.run(arguments: ["migrate"], stdin: "n\n") { _ in }
        XCTAssertEqual(runner.stdin, "n\n")
    }
}

private final class CountingRunner: HubCommandRunning {
    private(set) var calls = 0
    private(set) var stdin: String?

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        calls += 1
        completion(.success(""))
    }

    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        self.stdin = stdin
        run(arguments: arguments, completion: completion)
    }
}
