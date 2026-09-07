import Foundation

#if canImport(FoundationNetworking)
  import FoundationNetworking
  import CurrentHubCurlShim
#endif

#if canImport(Security)
  import Security
#endif

public struct CurrentHubHTTPResponse: Sendable {
  public let statusCode: Int
  public let headers: [String: String]
  public let body: Data
  public let finalURL: URL

  public init(statusCode: Int, headers: [String: String], body: Data, finalURL: URL) {
    self.statusCode = statusCode
    self.headers = headers
    self.body = body
    self.finalURL = finalURL
  }

  func header(_ name: String) -> String? {
    headers.first { $0.key.caseInsensitiveCompare(name) == .orderedSame }?.value
  }
}

public protocol CurrentHubHTTPTransport: Sendable {
  func send(_ request: URLRequest) async throws -> CurrentHubHTTPResponse
}

/// A transport that validates the invitation's DER leaf-certificate SHA-256
/// on the same TLS connection before transmitting the claim body.
public protocol CurrentHubInvitationPinningTransport: CurrentHubHTTPTransport {
  func send(
    _ request: URLRequest,
    validatingLeafCertificateSHA256 expectedLeafCertificateSHA256: String
  ) async throws -> CurrentHubHTTPResponse
}

public struct CurrentHubURLSessionTransport: CurrentHubInvitationPinningTransport {
  public static let defaultMaximumResponseBytes = 1_048_576

  private let session: URLSession
  private let maximumResponseBytes: Int
  private let usesInjectedURLSession: Bool
  private let configuredLeafCertificateSHA256: String?
  #if canImport(Security)
    private let trustedCertificateAuthoritiesDER: [Data]
  #endif

  public init(maximumResponseBytes: Int = Self.defaultMaximumResponseBytes) {
    precondition(maximumResponseBytes > 0)
    session = URLSession(
      configuration: Self.isolatedConfiguration(),
      delegate: CurrentHubRejectRedirectsDelegate(),
      delegateQueue: nil
    )
    self.maximumResponseBytes = maximumResponseBytes
    usesInjectedURLSession = false
    configuredLeafCertificateSHA256 = nil
    #if canImport(Security)
      trustedCertificateAuthoritiesDER = []
    #endif
  }

  public init(
    maximumResponseBytes: Int = Self.defaultMaximumResponseBytes,
    trustedCertificateAuthoritiesDER: [Data],
    expectedLeafCertificateSHA256: String? = nil
  ) throws {
    precondition(maximumResponseBytes > 0)
    if let expectedLeafCertificateSHA256 {
      guard expectedLeafCertificateSHA256.utf8.count == 64,
        expectedLeafCertificateSHA256.utf8.allSatisfy({
          (48...57).contains($0) || (97...102).contains($0)
        })
      else {
        throw CurrentHubError.invalidRequest(
          "expected leaf certificate SHA-256 must be 64 lowercase hexadecimal characters"
        )
      }
    }
    #if canImport(Security)
      let anchors = try trustedCertificateAuthoritiesDER.map { data in
        guard let certificate = SecCertificateCreateWithData(nil, data as CFData) else {
          throw CurrentHubError.invalidRequest("trusted certificate authority is not valid DER")
        }
        return certificate
      }
      session = URLSession(
        configuration: Self.isolatedConfiguration(),
        delegate: CurrentHubRejectRedirectsDelegate(
          anchors: anchors,
          expectedLeafSHA256: expectedLeafCertificateSHA256
        ),
        delegateQueue: nil
      )
      usesInjectedURLSession = false
      self.trustedCertificateAuthoritiesDER = trustedCertificateAuthoritiesDER
    #else
      guard trustedCertificateAuthoritiesDER.isEmpty else {
        throw CurrentHubError.invalidRequest(
          "explicit certificate anchors require a Security-backed platform; configure the isolated process trust store on FoundationNetworking"
        )
      }
      session = URLSession(
        configuration: Self.isolatedConfiguration(),
        delegate: CurrentHubRejectRedirectsDelegate(),
        delegateQueue: nil
      )
      usesInjectedURLSession = false
    #endif
    self.maximumResponseBytes = maximumResponseBytes
    configuredLeafCertificateSHA256 = expectedLeafCertificateSHA256
  }

  public init(
    maximumResponseBytes: Int = Self.defaultMaximumResponseBytes,
    expectedLeafCertificateSHA256: String
  ) throws {
    try self.init(
      maximumResponseBytes: maximumResponseBytes,
      trustedCertificateAuthoritiesDER: [],
      expectedLeafCertificateSHA256: expectedLeafCertificateSHA256
    )
  }

  init(
    configuration: URLSessionConfiguration,
    maximumResponseBytes: Int = Self.defaultMaximumResponseBytes
  ) {
    precondition(maximumResponseBytes > 0)
    session = URLSession(
      configuration: configuration,
      delegate: CurrentHubRejectRedirectsDelegate(),
      delegateQueue: nil
    )
    self.maximumResponseBytes = maximumResponseBytes
    usesInjectedURLSession = true
    configuredLeafCertificateSHA256 = nil
    #if canImport(Security)
      trustedCertificateAuthoritiesDER = []
    #endif
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

  public func send(_ request: URLRequest) async throws -> CurrentHubHTTPResponse {
    let data: Data
    let http: HTTPURLResponse
    do {
      #if canImport(FoundationNetworking)
        if usesInjectedURLSession {
          (data, http) = try await CurrentHubBoundedDataLoader(
            configuration: session.configuration,
            maximumResponseBytes: maximumResponseBytes
          ).load(request)
        } else {
          return try await CurrentHubCurlOperation(
            request: request,
            maximumResponseBytes: maximumResponseBytes,
            expectedLeafCertificateSHA256: configuredLeafCertificateSHA256
          ).load()
        }
      #else
        let (bytes, response) = try await session.bytes(for: request)
        guard let response = response as? HTTPURLResponse else {
          throw CurrentHubError.invalidResponse(
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
    } catch let error as CurrentHubError {
      throw error
    } catch is CancellationError {
      throw CancellationError()
    } catch let error as URLError where error.code == .cancelled && Task.isCancelled {
      throw CancellationError()
    } catch let error as URLError {
      throw CurrentHubError.transportFailure(code: error.errorCode)
    } catch {
      let failure = error as NSError
      throw CurrentHubError.transportFailure(code: failure.code)
    }

    guard let finalURL = http.url else {
      throw CurrentHubError.invalidResponse(
        statusCode: http.statusCode,
        requestID: http.value(forHTTPHeaderField: "X-Request-ID"),
        reason: "HTTP response URL is unavailable"
      )
    }

    var headers: [String: String] = [:]
    for (key, value) in http.allHeaderFields {
      headers[String(describing: key)] = String(describing: value)
    }
    return CurrentHubHTTPResponse(
      statusCode: http.statusCode,
      headers: headers,
      body: data,
      finalURL: finalURL
    )
  }

  public func send(
    _ request: URLRequest,
    validatingLeafCertificateSHA256 expectedLeafCertificateSHA256: String
  ) async throws -> CurrentHubHTTPResponse {
    try Self.validateLeafPin(expectedLeafCertificateSHA256)
    #if canImport(Security)
      guard !usesInjectedURLSession else {
        throw CurrentHubError.invalidRequest(
          "injected URLSession transport cannot own invitation leaf-pin validation"
        )
      }
      return try await CurrentHubURLSessionTransport(
        maximumResponseBytes: maximumResponseBytes,
        trustedCertificateAuthoritiesDER: trustedCertificateAuthoritiesDER,
        expectedLeafCertificateSHA256: expectedLeafCertificateSHA256
      ).send(request)
    #elseif canImport(FoundationNetworking)
      guard !usesInjectedURLSession else {
        throw CurrentHubError.invalidRequest(
          "injected URLSession transport cannot own invitation leaf-pin validation"
        )
      }
      return try await CurrentHubCurlOperation(
        request: request,
        maximumResponseBytes: maximumResponseBytes,
        expectedLeafCertificateSHA256: expectedLeafCertificateSHA256
      ).load()
    #else
      throw CurrentHubError.invalidRequest(
        "invitation leaf-pin validation is unavailable on this platform"
      )
    #endif
  }

  private static func validateLeafPin(_ value: String) throws {
    guard value.utf8.count == 64,
      value.utf8.allSatisfy({
        (48...57).contains($0) || (97...102).contains($0)
      })
    else {
      throw CurrentHubError.invalidRequest(
        "expected leaf certificate SHA-256 must be 64 lowercase hexadecimal characters"
      )
    }
  }

  private func responseTooLarge(_ response: HTTPURLResponse) -> CurrentHubError {
    .invalidResponse(
      statusCode: response.statusCode,
      requestID: response.value(forHTTPHeaderField: "X-Request-ID"),
      reason: "response body exceeds \(maximumResponseBytes) bytes"
    )
  }
}

struct CurrentHubBoundedBodyAccumulator {
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
  private final class CurrentHubCurlOperation: @unchecked Sendable {
    private let handle: OpaquePointer
    private let maximumResponseBytes: Int

    init(
      request: URLRequest,
      maximumResponseBytes: Int,
      expectedLeafCertificateSHA256: String?
    ) throws {
      guard let url = request.url?.absoluteString,
        let method = request.httpMethod,
        request.httpBodyStream == nil
      else {
        throw CurrentHubError.invalidRequest(
          "Linux transport requires a URL, an HTTP method, and an in-memory request body"
        )
      }
      let body = request.httpBody ?? Data()
      let timeoutSeconds = min(request.timeoutInterval, 30)
      guard timeoutSeconds > 0,
        timeoutSeconds <= Double(Int.max) / 1_000
      else {
        throw CurrentHubError.invalidRequest("request timeout must be positive and finite")
      }
      let timeoutMilliseconds = max(1, Int(timeoutSeconds * 1_000))
      guard let handle = url.withCString({ urlPointer in
        method.withCString { methodPointer in
          body.withUnsafeBytes { bytes in
            current_hub_curl_operation_create(
              urlPointer,
              methodPointer,
              bytes.bindMemory(to: UInt8.self).baseAddress,
              bytes.count,
              maximumResponseBytes,
              timeoutMilliseconds
            )
          }
        }
      }) else {
        throw CurrentHubError.transportFailure(code: 2)
      }
      self.handle = handle
      self.maximumResponseBytes = maximumResponseBytes

      if let expectedLeafCertificateSHA256 {
        guard expectedLeafCertificateSHA256.withCString({
          current_hub_curl_operation_set_expected_leaf_sha256(handle, $0)
        }) != 0 else {
          throw CurrentHubError.invalidRequest(
            "expected leaf certificate SHA-256 must be 64 lowercase hexadecimal characters"
          )
        }
      }

      for (name, value) in request.allHTTPHeaderFields ?? [:] {
        let header = "\(name): \(value)"
        guard header.withCString({
          current_hub_curl_operation_add_header(handle, $0)
        }) != 0 else {
          throw CurrentHubError.transportFailure(code: 27)
        }
      }
    }

    deinit {
      current_hub_curl_operation_destroy(handle)
    }

    func cancel() {
      current_hub_curl_operation_cancel(handle)
    }

    func load() async throws -> CurrentHubHTTPResponse {
      if Task.isCancelled {
        cancel()
        throw CancellationError()
      }
      return try await withTaskCancellationHandler {
        try await Task.detached { [self] in
          try perform()
        }.value
      } onCancel: { [self] in
        cancel()
      }
    }

    private func perform() throws -> CurrentHubHTTPResponse {
      let code = current_hub_curl_operation_perform(handle)
      let statusCode = Int(current_hub_curl_operation_status_code(handle))
      if current_hub_curl_operation_was_cancelled(handle) != 0 {
        throw CancellationError()
      }
      if current_hub_curl_operation_response_too_large(handle) != 0 {
        throw CurrentHubError.invalidResponse(
          statusCode: statusCode,
          requestID: nil,
          reason: "response body exceeds \(maximumResponseBytes) bytes"
        )
      }
      if current_hub_curl_operation_headers_too_large(handle) != 0 {
        throw CurrentHubError.invalidResponse(
          statusCode: statusCode,
          requestID: nil,
          reason: "response headers exceed 65536 bytes"
        )
      }
      guard code == 0 else {
        throw CurrentHubError.transportFailure(code: Int(code))
      }
      guard let effectiveURLPointer = current_hub_curl_operation_effective_url(handle),
        let effectiveURL = URL(string: String(cString: effectiveURLPointer))
      else {
        throw CurrentHubError.invalidResponse(
          statusCode: statusCode,
          requestID: nil,
          reason: "HTTP response URL is unavailable"
        )
      }
      let bodyLength = current_hub_curl_operation_response_body_length(handle)
      let body: Data
      if bodyLength == 0 {
        body = Data()
      } else {
        guard let pointer = current_hub_curl_operation_response_body(handle) else {
          throw CurrentHubError.transportFailure(code: 27)
        }
        body = Data(bytes: pointer, count: bodyLength)
      }
      let headers = try parseHeaders(statusCode: statusCode)
      return CurrentHubHTTPResponse(
        statusCode: statusCode,
        headers: headers,
        body: body,
        finalURL: effectiveURL
      )
    }

    private func parseHeaders(statusCode: Int) throws -> [String: String] {
      let length = current_hub_curl_operation_response_headers_length(handle)
      guard length == 0 || current_hub_curl_operation_response_headers(handle) != nil else {
        throw CurrentHubError.transportFailure(code: 27)
      }
      guard length > 0,
        let pointer = current_hub_curl_operation_response_headers(handle)
      else { return [:] }
      let raw = String(decoding: UnsafeBufferPointer(start: pointer, count: length), as: UTF8.self)
      var currentStatus: Int?
      var currentHeaders: [String: String] = [:]
      var completedBlocks: [(status: Int, headers: [String: String])] = []
      for line in raw.components(separatedBy: "\r\n") {
        if line.hasPrefix("HTTP/") {
          if let currentStatus {
            completedBlocks.append((currentStatus, currentHeaders))
          }
          guard let status = line.split(separator: " ", omittingEmptySubsequences: true)
            .dropFirst().first.flatMap({ Int($0) })
          else {
            throw invalidHeaderResponse(statusCode, "malformed HTTP status line")
          }
          currentStatus = status
          currentHeaders = [:]
          continue
        }
        if line.isEmpty {
          if let completedStatus = currentStatus {
            completedBlocks.append((completedStatus, currentHeaders))
            currentStatus = nil
            currentHeaders = [:]
          }
          continue
        }
        // Header callback lines after a completed response block are trailers.
        // They are deliberately ignored and cannot replace final response metadata.
        guard currentStatus != nil else { continue }
        guard let colon = line.firstIndex(of: ":") else {
          throw invalidHeaderResponse(statusCode, "malformed response header")
        }
        let name = String(line[..<colon]).trimmingCharacters(in: .whitespaces)
          .lowercased()
        let value = String(line[line.index(after: colon)...])
          .trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty else {
          throw invalidHeaderResponse(statusCode, "malformed response header")
        }
        if let existing = currentHeaders[name] {
          if Self.singletonHeaders.contains(name), existing != value {
            throw invalidHeaderResponse(
              statusCode,
              "conflicting \(name) response headers"
            )
          }
          if existing != value { currentHeaders[name] = "\(existing), \(value)" }
        } else {
          currentHeaders[name] = value
        }
      }
      if let currentStatus {
        completedBlocks.append((currentStatus, currentHeaders))
      }
      guard let final = completedBlocks.last(where: { $0.status >= 200 }),
        final.status == statusCode
      else {
        throw invalidHeaderResponse(statusCode, "final response header block is unavailable")
      }
      return final.headers
    }

    private func invalidHeaderResponse(_ statusCode: Int, _ reason: String) -> CurrentHubError {
      .invalidResponse(statusCode: statusCode, requestID: nil, reason: reason)
    }

    private static let singletonHeaders: Set<String> = [
      "content-length", "content-type", "etag", "location", "x-request-id",
    ]
  }

  private final class CurrentHubBoundedDataLoader: NSObject, URLSessionDataDelegate,
    @unchecked Sendable
  {
    private let configuration: URLSessionConfiguration
    private let maximumResponseBytes: Int
    private let lock = NSLock()
    private var accumulator: CurrentHubBoundedBodyAccumulator
    private var response: HTTPURLResponse?
    private var continuation: CheckedContinuation<(Data, HTTPURLResponse), Error>?
    private var session: URLSession?
    private var task: URLSessionDataTask?
    private var terminalError: Error?
    private var cancelled = false

    init(configuration: URLSessionConfiguration, maximumResponseBytes: Int) {
      self.configuration = configuration
      self.maximumResponseBytes = maximumResponseBytes
      accumulator = CurrentHubBoundedBodyAccumulator(
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
          terminalError = CurrentHubError.invalidResponse(
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
            ?? CurrentHubError.invalidResponse(
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
            CurrentHubError.invalidResponse(
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

    private func responseTooLarge(_ response: HTTPURLResponse) -> CurrentHubError {
      .invalidResponse(
        statusCode: response.statusCode,
        requestID: response.value(forHTTPHeaderField: "X-Request-ID"),
        reason: "response body exceeds \(maximumResponseBytes) bytes"
      )
    }
  }
#endif

final class CurrentHubRejectRedirectsDelegate: NSObject, URLSessionTaskDelegate,
  @unchecked Sendable
{
  #if canImport(Security)
    private let anchors: [SecCertificate]
    private let expectedLeafSHA256: String?

    init(anchors: [SecCertificate] = [], expectedLeafSHA256: String? = nil) {
      self.anchors = anchors
      self.expectedLeafSHA256 = expectedLeafSHA256
    }

    func urlSession(
      _ session: URLSession,
      didReceive challenge: URLAuthenticationChallenge,
      completionHandler: @escaping @Sendable (
        URLSession.AuthChallengeDisposition, URLCredential?
      ) -> Void
    ) {
      handle(challenge, completionHandler: completionHandler)
    }

    func urlSession(
      _ session: URLSession,
      task: URLSessionTask,
      didReceive challenge: URLAuthenticationChallenge,
      completionHandler: @escaping @Sendable (
        URLSession.AuthChallengeDisposition, URLCredential?
      ) -> Void
    ) {
      handle(challenge, completionHandler: completionHandler)
    }

    private func handle(
      _ challenge: URLAuthenticationChallenge,
      completionHandler: @escaping @Sendable (
        URLSession.AuthChallengeDisposition, URLCredential?
      ) -> Void
    ) {
      guard challenge.protectionSpace.authenticationMethod
        == NSURLAuthenticationMethodServerTrust,
        let trust = challenge.protectionSpace.serverTrust
      else {
        completionHandler(.performDefaultHandling, nil)
        return
      }
      if !anchors.isEmpty {
        SecTrustSetAnchorCertificates(trust, anchors as CFArray)
        SecTrustSetAnchorCertificatesOnly(trust, true)
      }
      var error: CFError?
      guard SecTrustEvaluateWithError(trust, &error) else {
        completionHandler(.cancelAuthenticationChallenge, nil)
        return
      }
      if let expectedLeafSHA256 {
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate],
          let leaf = chain.first,
          CurrentHubSHA256.hexDigest(of: SecCertificateCopyData(leaf) as Data)
            == expectedLeafSHA256
        else {
          completionHandler(.cancelAuthenticationChallenge, nil)
          return
        }
      }
      completionHandler(.useCredential, URLCredential(trust: trust))
    }
  #endif

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
