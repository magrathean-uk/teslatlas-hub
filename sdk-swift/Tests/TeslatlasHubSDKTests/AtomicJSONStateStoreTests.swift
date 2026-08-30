import Foundation
import XCTest

@testable import TeslatlasHubSDK

final class AtomicJSONStateStoreTests: XCTestCase {
  private struct TestState: Codable, Equatable, Sendable {
    let phase: String
    let completedBytes: Int
  }

  func testSavedStateReloadsThroughAFreshStoreInstance() async throws {
    let fixture = try TemporaryStateFixture()
    defer { fixture.remove() }

    let firstStore = AtomicJSONStateStore<TestState>(fileURL: fixture.fileURL)
    let initialState = try await firstStore.load()
    XCTAssertNil(initialState)

    let expected = TestState(phase: "claiming", completedBytes: 4_096)
    try await firstStore.save(expected)

    let relaunchedStore = AtomicJSONStateStore<TestState>(fileURL: fixture.fileURL)
    let reloadedState = try await relaunchedStore.load()
    XCTAssertEqual(reloadedState, expected)
  }

  func testRemoveIsIdempotentAndClearsPersistedState() async throws {
    let fixture = try TemporaryStateFixture()
    defer { fixture.remove() }

    let store = AtomicJSONStateStore<TestState>(fileURL: fixture.fileURL)
    try await store.save(TestState(phase: "downloading", completedBytes: 12))

    try await store.remove()
    try await store.remove()

    let removedState = try await store.load()
    XCTAssertNil(removedState)
    XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.fileURL.path))
  }
}

private struct TemporaryStateFixture {
  let directoryURL: URL
  let fileURL: URL

  init() throws {
    directoryURL = FileManager.default.temporaryDirectory
      .appendingPathComponent(UUID().uuidString, isDirectory: true)
    fileURL =
      directoryURL
      .appendingPathComponent("nested", isDirectory: true)
      .appendingPathComponent("state.json", isDirectory: false)
  }

  func remove() {
    try? FileManager.default.removeItem(at: directoryURL)
  }
}
