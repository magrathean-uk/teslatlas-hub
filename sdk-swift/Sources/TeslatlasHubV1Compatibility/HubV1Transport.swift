import Foundation

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

struct HubV1HTTPResponse: Sendable {
  let statusCode: Int
  let headers: [String: String]
  let body: Data
  let finalURL: URL

  func header(_ name: String) -> String? {
    headers.first { $0.key.caseInsensitiveCompare(name) == .orderedSame }?.value
  }
}

protocol HubV1HTTPTransport: Sendable {
  func send(_ request: URLRequest) async throws -> HubV1HTTPResponse
}

struct HubV1URLSessionTransport: HubV1HTTPTransport {
  static let defaultMaximumResponseBytes = 16 * 1_024 * 1_024

  private let session: URLSession
  private let maximumResponseBytes: Int

  init(maximumResponseBytes: Int = Self.defaultMaximumResponseBytes) {
    self.init(
      configuration: Self.isolatedConfiguration(),
      maximumResponseBytes: maximumResponseBytes
    )
  }

  init(
    configuration: URLSessionConfiguration,
    maximumResponseBytes: Int = Self.defaultMaximumResponseBytes
  ) {
    precondition(maximumResponseBytes > 0)
    session = URLSession(
      configuration: configuration,
      delegate: HubV1RejectRedirectsDelegate(),
      delegateQueue: nil
    )
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
    configuration.timeoutIntervalForRequest = 20
    configuration.timeoutIntervalForResource = 30
    return configuration
  }

  func send(_ request: URLRequest) async throws -> HubV1HTTPResponse {
    let data: Data
    let http: HTTPURLResponse
    do {
      #if canImport(FoundationNetworking)
        (data, http) = try await HubV1BoundedDataLoader(
          configuration: session.configuration,
          maximumResponseBytes: maximumResponseBytes
        ).load(request)
      #else
        let (bytes, response) = try await session.bytes(for: request)
        guard let response = response as? HTTPURLResponse else {
          throw HubV1Error.invalidResponse(
            statusCode: 0,
            requestID: nil,
            reason: "transport returned a non-HTTP response"
          )
        }
        if response.expectedContentLength > maximumResponseBytes {
          throw responseTooLarge(response)
        }
        var received = Data()
        if response.expectedContentLength > 0 {
          received.reserveCapacity(Int(response.expectedContentLength))
        }
        for try await byte in bytes {
          guard received.count < maximumResponseBytes else {
            throw responseTooLarge(response)
          }
          received.append(byte)
        }
        data = received
        http = response
      #endif
    } catch let error as HubV1Error {
      throw error
    } catch let error as URLError {
      throw HubV1Error.transportFailure(code: error.errorCode)
    } catch {
      throw HubV1Error.transportFailure(code: -1)
    }

    guard let finalURL = http.url else {
      throw HubV1Error.invalidResponse(
        statusCode: http.statusCode,
        requestID: http.value(forHTTPHeaderField: "X-Request-ID"),
        reason: "HTTP response URL is unavailable"
      )
    }

    var headers: [String: String] = [:]
    for (key, value) in http.allHeaderFields {
      headers[String(describing: key)] = String(describing: value)
    }
    return HubV1HTTPResponse(
      statusCode: http.statusCode,
      headers: headers,
      body: data,
      finalURL: finalURL
    )
  }

  private func responseTooLarge(_ response: HTTPURLResponse) -> HubV1Error {
    .invalidResponse(
      statusCode: response.statusCode,
      requestID: response.value(forHTTPHeaderField: "X-Request-ID"),
      reason: "response body exceeds \(maximumResponseBytes) bytes"
    )
  }
}

struct HubV1BoundedBodyAccumulator {
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
  private final class HubV1BoundedDataLoader: NSObject, URLSessionDataDelegate,
    @unchecked Sendable
  {
    private let configuration: URLSessionConfiguration
    private let maximumResponseBytes: Int
    private let lock = NSLock()
    private var accumulator: HubV1BoundedBodyAccumulator
    private var response: HTTPURLResponse?
    private var continuation: CheckedContinuation<(Data, HTTPURLResponse), Error>?
    private var session: URLSession?
    private var task: URLSessionDataTask?
    private var terminalError: Error?
    private var cancelled = false

    init(configuration: URLSessionConfiguration, maximumResponseBytes: Int) {
      self.configuration = configuration
      self.maximumResponseBytes = maximumResponseBytes
      accumulator = HubV1BoundedBodyAccumulator(
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
          terminalError = HubV1Error.invalidResponse(
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
            ?? HubV1Error.invalidResponse(
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

    func urlSession(
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
            HubV1Error.invalidResponse(
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

    func urlSession(
      _ session: URLSession,
      task: URLSessionTask,
      willPerformHTTPRedirection response: HTTPURLResponse,
      newRequest request: URLRequest,
      completionHandler: @escaping @Sendable (URLRequest?) -> Void
    ) {
      completionHandler(nil)
    }

    private func responseTooLarge(_ response: HTTPURLResponse) -> HubV1Error {
      .invalidResponse(
        statusCode: response.statusCode,
        requestID: response.value(forHTTPHeaderField: "X-Request-ID"),
        reason: "response body exceeds \(maximumResponseBytes) bytes"
      )
    }
  }
#endif

final class HubV1RejectRedirectsDelegate: NSObject, URLSessionTaskDelegate,
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
}
