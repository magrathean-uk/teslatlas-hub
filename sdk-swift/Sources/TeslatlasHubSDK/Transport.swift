import Foundation

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

public struct TeslatlasHTTPResponse: Equatable, Sendable {
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

public protocol TeslatlasHTTPTransport: Sendable {
  func send(_ request: URLRequest) async throws -> TeslatlasHTTPResponse
}

public struct URLSessionTeslatlasTransport: TeslatlasHTTPTransport {
  public static let defaultMaximumResponseBytes = 16 * 1_024 * 1_024

  private let session: URLSession
  private let maximumResponseBytes: Int

  public init(
    maximumResponseBytes: Int = Self.defaultMaximumResponseBytes
  ) {
    self.init(
      session: URLSession(
        configuration: Self.isolatedConfiguration(),
        delegate: TeslatlasRejectRedirectsDelegate(),
        delegateQueue: nil
      ),
      maximumResponseBytes: maximumResponseBytes
    )
  }

  public init(
    session: URLSession,
    maximumResponseBytes: Int = Self.defaultMaximumResponseBytes
  ) {
    precondition(maximumResponseBytes > 0)
    self.session = session
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

  public func send(_ request: URLRequest) async throws -> TeslatlasHTTPResponse {
    #if canImport(FoundationNetworking)
      let (data, httpResponse) = try await TeslatlasBoundedDataLoader(
        configuration: session.configuration,
        maximumResponseBytes: maximumResponseBytes
      ).load(request)
    #else
      let (bytes, response) = try await session.bytes(for: request)
      guard let httpResponse = response as? HTTPURLResponse else {
        throw TeslatlasSDKError.invalidResponse(
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
    return TeslatlasHTTPResponse(
      statusCode: httpResponse.statusCode,
      headers: headers,
      body: data
    )
  }

  private func responseTooLarge(statusCode: Int, requestID: String?)
    -> TeslatlasSDKError
  {
    .invalidResponse(
      statusCode: statusCode,
      requestID: requestID,
      reason: "response body exceeds \(maximumResponseBytes) bytes"
    )
  }
}

class TeslatlasRejectRedirectsDelegate: NSObject, URLSessionTaskDelegate,
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

struct TeslatlasBoundedBodyAccumulator {
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
  private final class TeslatlasBoundedDataLoader:
    TeslatlasRejectRedirectsDelegate, URLSessionDataDelegate, @unchecked Sendable
  {
    private let configuration: URLSessionConfiguration
    private let maximumResponseBytes: Int
    private let lock = NSLock()
    private var accumulator: TeslatlasBoundedBodyAccumulator
    private var response: HTTPURLResponse?
    private var continuation: CheckedContinuation<(Data, HTTPURLResponse), Error>?
    private var session: URLSession?
    private var task: URLSessionDataTask?
    private var terminalError: Error?
    private var cancelled = false

    init(configuration: URLSessionConfiguration, maximumResponseBytes: Int) {
      self.configuration = configuration
      self.maximumResponseBytes = maximumResponseBytes
      accumulator = TeslatlasBoundedBodyAccumulator(
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
          terminalError = TeslatlasSDKError.invalidResponse(
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
            ?? TeslatlasSDKError.invalidResponse(
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
            TeslatlasSDKError.invalidResponse(
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
      -> TeslatlasSDKError
    {
      .invalidResponse(
        statusCode: response.statusCode,
        requestID: response.value(forHTTPHeaderField: "X-Request-ID"),
        reason: "response body exceeds \(maximumResponseBytes) bytes"
      )
    }
  }
#endif

public protocol TeslatlasAuthorization: Sendable {
  func apply(to request: inout URLRequest)
}

public struct BearerCredential: TeslatlasAuthorization, Sendable,
  CustomStringConvertible, CustomDebugStringConvertible
{
  private let token: String

  public init(_ token: String) throws {
    guard !token.isEmpty,
      token.unicodeScalars.allSatisfy({
        !CharacterSet.controlCharacters.contains($0)
      })
    else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "bearer credential is empty or contains control characters"
      )
    }
    self.token = token
  }

  public var description: String { "BearerCredential(<redacted>)" }
  public var debugDescription: String { description }

  public func apply(to request: inout URLRequest) {
    request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
  }
}
