import Foundation

actor AtomicJSONStateStore<State: Codable & Sendable> {
  private let fileURL: URL

  init(fileURL: URL) {
    self.fileURL = fileURL
  }

  func load() throws -> State? {
    guard FileManager.default.fileExists(atPath: fileURL.path) else {
      return nil
    }

    let data = try Data(contentsOf: fileURL)
    return try JSONDecoder().decode(State.self, from: data)
  }

  func save(_ state: State) throws {
    let directoryURL = fileURL.deletingLastPathComponent()
    try FileManager.default.createDirectory(
      at: directoryURL,
      withIntermediateDirectories: true
    )

    let data = try JSONEncoder().encode(state)
    try data.write(to: fileURL, options: .atomic)
  }

  func remove() throws {
    do {
      try FileManager.default.removeItem(at: fileURL)
    } catch let error as CocoaError where error.code == .fileNoSuchFile {
      return
    }
  }
}
