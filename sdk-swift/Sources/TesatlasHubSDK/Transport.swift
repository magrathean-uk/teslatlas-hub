import Foundation

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

public struct TesatlasHTTPResponse: Equatable, Sendable {
  public let statusCode: Int
  public let headers: [String: String]
  public let body: Data

  public init(statusCode: Int, headers: [String: String], body: Data) {
    self.statusCode = statusCode
    self.headers = headers
    self.body = body
  }

  public func header(_ name: String) -> String? {
    headers.first { $0.key.caseInsensitiveCompare(name) == .orderedSame }?.value
  }
}

public protocol TesatlasHTTPTransport: Sendable {
  func send(_ request: URLRequest) async throws -> TesatlasHTTPResponse
}

public struct URLSessionTesatlasTransport: TesatlasHTTPTransport {
  public static let defaultMaximumResponseBytes = 16 * 1_024 * 1_024

  // Value copies share the lifetime owner. Caller-injected sessions are borrowed.
  private let sessionOwner: TesatlasSessionOwner
  private var session: URLSession { sessionOwner.session }
  private let maximumResponseBytes: Int

  public init(
    maximumResponseBytes: Int = Self.defaultMaximumResponseBytes
  ) {
    precondition(maximumResponseBytes > 0)
    sessionOwner = TesatlasSessionOwner(
      session: URLSession(
        configuration: Self.isolatedConfiguration(),
        delegate: TesatlasRejectRedirectsDelegate(),
        delegateQueue: nil
      ),
      owned: true
    )
    self.maximumResponseBytes = maximumResponseBytes
  }

  public init(
    session: URLSession,
    maximumResponseBytes: Int = Self.defaultMaximumResponseBytes
  ) {
    precondition(maximumResponseBytes > 0)
    sessionOwner = TesatlasSessionOwner(session: session, owned: false)
    self.maximumResponseBytes = maximumResponseBytes
  }

  static func isolatedConfiguration() -> URLSessionConfiguration {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.httpShouldSetCookies = false
    configuration.httpCookieAcceptPolicy = .never
    configuration.httpCookieStorage = nil
    configuration.urlCredentialStorage = nil
    configuration.urlCache = nil
    configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
    return configuration
  }

  public func send(_ request: URLRequest) async throws -> TesatlasHTTPResponse {
    defer { withExtendedLifetime(sessionOwner) {} }
    #if canImport(FoundationNetworking)
      let (data, httpResponse) = try await TesatlasBoundedDataLoader(
        configuration: session.configuration,
        maximumResponseBytes: maximumResponseBytes
      ).load(request)
    #else
      let (bytes, response) = try await session.bytes(for: request)
      guard let httpResponse = response as? HTTPURLResponse else {
        throw TesatlasSDKError.invalidResponse(
          statusCode: 0,
          requestID: nil,
          reason: "transport returned a non-HTTP response"
        )
      }
      let requestID = httpResponse.value(forHTTPHeaderField: "X-Request-ID")
      if httpResponse.expectedContentLength > maximumResponseBytes {
        throw responseTooLarge(
          statusCode: httpResponse.statusCode,
          requestID: requestID
        )
      }

      var data = Data()
      if httpResponse.expectedContentLength > 0 {
        data.reserveCapacity(Int(httpResponse.expectedContentLength))
      }
      for try await byte in bytes {
        guard data.count < maximumResponseBytes else {
          throw responseTooLarge(
            statusCode: httpResponse.statusCode,
            requestID: requestID
          )
        }
        data.append(byte)
      }
    #endif

    var headers: [String: String] = [:]
    for (key, value) in httpResponse.allHeaderFields {
      headers[String(describing: key)] = String(describing: value)
    }
    return TesatlasHTTPResponse(
      statusCode: httpResponse.statusCode,
      headers: headers,
      body: data
    )
  }

  private func responseTooLarge(statusCode: Int, requestID: String?)
    -> TesatlasSDKError
  {
    .invalidResponse(
      statusCode: statusCode,
      requestID: requestID,
      reason: "response body exceeds \(maximumResponseBytes) bytes"
    )
  }
}

private final class TesatlasSessionOwner: Sendable {
  let session: URLSession
  private let owned: Bool

  init(session: URLSession, owned: Bool) {
    self.session = session
    self.owned = owned
  }

  deinit {
    if owned { session.finishTasksAndInvalidate() }
  }
}

class TesatlasRejectRedirectsDelegate: NSObject, URLSessionTaskDelegate,
  @unchecked Sendable
{
  func urlSession(
    _ session: URLSession,
    task: URLSessionTask,
    willPerformHTTPRedirection response: HTTPURLResponse,
    newRequest request: URLRequest,
    completionHandler: @escaping @Sendable (URLRequest?) -> Void
  ) {
    completionHandler(nil)
  }

  func urlSession(
    _ session: URLSession,
    task: URLSessionTask,
    didCompleteWithError error: Error?
  ) {}
}

struct TesatlasBoundedBodyAccumulator {
  let maximumBytes: Int
  private(set) var data = Data()

  init(maximumBytes: Int) {
    precondition(maximumBytes > 0)
    self.maximumBytes = maximumBytes
  }

  mutating func append(_ chunk: Data) -> Bool {
    guard chunk.count <= maximumBytes - data.count else {
      return false
    }
    data.append(chunk)
    return true
  }
}

#if canImport(FoundationNetworking)
  private final class TesatlasBoundedDataLoader:
    TesatlasRejectRedirectsDelegate, URLSessionDataDelegate, @unchecked Sendable
  {
    private let configuration: URLSessionConfiguration
    private let maximumResponseBytes: Int
    private let lock = NSLock()
    private var accumulator: TesatlasBoundedBodyAccumulator
    private var response: HTTPURLResponse?
    private var continuation: CheckedContinuation<(Data, HTTPURLResponse), Error>?
    private var session: URLSession?
    private var task: URLSessionDataTask?
    private var terminalError: Error?
    private var cancelled = false

    init(configuration: URLSessionConfiguration, maximumResponseBytes: Int) {
      self.configuration = configuration
      self.maximumResponseBytes = maximumResponseBytes
      accumulator = TesatlasBoundedBodyAccumulator(
        maximumBytes: maximumResponseBytes
      )
    }

    func load(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
      try await withTaskCancellationHandler {
        try await withCheckedThrowingContinuation { continuation in
          start(request, continuation: continuation)
        }
      } onCancel: {
        cancel()
      }
    }

    private func start(
      _ request: URLRequest,
      continuation: CheckedContinuation<(Data, HTTPURLResponse), Error>
    ) {
      let session = URLSession(
        configuration: configuration,
        delegate: self,
        delegateQueue: nil
      )
      let task = session.dataTask(with: request)
      let wasCancelled = lock.withLock {
        self.continuation = continuation
        self.session = session
        self.task = task
        return cancelled
      }
      if wasCancelled {
        completeImmediatelyWithCancellation()
      } else {
        task.resume()
      }
    }

    private func cancel() {
      let task = lock.withLock {
        cancelled = true
        return self.task
      }
      task?.cancel()
    }

    private func completeImmediatelyWithCancellation() {
      let completion = lock.withLock {
        () -> CheckedContinuation<(Data, HTTPURLResponse), Error>? in
        let continuation = self.continuation
        self.continuation = nil
        return continuation
      }
      session?.invalidateAndCancel()
      completion?.resume(throwing: CancellationError())
    }

    func urlSession(
      _ session: URLSession,
      dataTask: URLSessionDataTask,
      didReceive response: URLResponse,
      completionHandler: @escaping @Sendable (URLSession.ResponseDisposition) -> Void
    ) {
      let disposition = lock.withLock { () -> URLSession.ResponseDisposition in
        guard terminalError == nil else { return .cancel }
        guard let response = response as? HTTPURLResponse else {
          terminalError = TesatlasSDKError.invalidResponse(
            statusCode: 0,
            requestID: nil,
            reason: "transport returned a non-HTTP response"
          )
          return .cancel
        }
        self.response = response
        if response.expectedContentLength > maximumResponseBytes {
          terminalError = responseTooLarge(response)
          return .cancel
        }
        return .allow
      }
      completionHandler(disposition)
    }

    func urlSession(
      _ session: URLSession,
      dataTask: URLSessionDataTask,
      didReceive data: Data
    ) {
      let shouldCancel = lock.withLock {
        guard terminalError == nil else { return true }
        guard accumulator.append(data) else {
          terminalError =
            response.map(responseTooLarge)
            ?? TesatlasSDKError.invalidResponse(
              statusCode: 0,
              requestID: nil,
              reason: "response body exceeds \(maximumResponseBytes) bytes"
            )
          return true
        }
        return false
      }
      if shouldCancel {
        dataTask.cancel()
      }
    }

    override func urlSession(
      _ session: URLSession,
      task: URLSessionTask,
      didCompleteWithError error: Error?
    ) {
      let completion = lock.withLock {
        () -> (
          CheckedContinuation<(Data, HTTPURLResponse), Error>,
          Result<(Data, HTTPURLResponse), Error>
        )? in
        guard let continuation else { return nil }
        self.continuation = nil

        let result: Result<(Data, HTTPURLResponse), Error>
        if let terminalError {
          result = .failure(terminalError)
        } else if cancelled {
          result = .failure(CancellationError())
        } else if let error {
          result = .failure(error)
        } else if let response {
          result = .success((accumulator.data, response))
        } else {
          result = .failure(
            TesatlasSDKError.invalidResponse(
              statusCode: 0,
              requestID: nil,
              reason: "transport returned a non-HTTP response"
            )
          )
        }
        return (continuation, result)
      }

      session.finishTasksAndInvalidate()
      guard let completion else { return }
      switch completion.1 {
      case .success(let output):
        completion.0.resume(returning: output)
      case .failure(let error):
        completion.0.resume(throwing: error)
      }
    }

    private func responseTooLarge(_ response: HTTPURLResponse)
      -> TesatlasSDKError
    {
      .invalidResponse(
        statusCode: response.statusCode,
        requestID: response.value(forHTTPHeaderField: "X-Request-ID"),
        reason: "response body exceeds \(maximumResponseBytes) bytes"
      )
    }
  }
#endif

public protocol TesatlasAuthorization: Sendable {
  func apply(to request: inout URLRequest)
}

public struct BearerCredential: TesatlasAuthorization, Sendable,
  CustomStringConvertible, CustomDebugStringConvertible, CustomReflectable
{
  private let token: String

  public init(_ token: String) throws {
    guard !token.isEmpty,
      token.unicodeScalars.allSatisfy({
        !CharacterSet.controlCharacters.contains($0)
      })
    else {
      throw TesatlasSDKError.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "bearer credential is empty or contains control characters"
      )
    }
    self.token = token
  }

  public var description: String { "BearerCredential(<redacted>)" }
  public var debugDescription: String { description }
  public var customMirror: Mirror { Mirror(self, children: [:]) }

  public func apply(to request: inout URLRequest) {
    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
  }
}
