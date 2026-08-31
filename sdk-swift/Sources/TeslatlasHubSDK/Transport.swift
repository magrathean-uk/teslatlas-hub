import Foundation

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
      session: URLSession(configuration: Self.isolatedConfiguration()),
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
