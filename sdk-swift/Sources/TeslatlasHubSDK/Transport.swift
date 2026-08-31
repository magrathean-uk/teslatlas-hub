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
  private let session: URLSession

  public init(session: URLSession = .shared) {
    self.session = session
  }

  public func send(_ request: URLRequest) async throws -> TeslatlasHTTPResponse {
    let (data, response) = try await session.data(for: request)
    guard let httpResponse = response as? HTTPURLResponse else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "transport returned a non-HTTP response"
      )
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
