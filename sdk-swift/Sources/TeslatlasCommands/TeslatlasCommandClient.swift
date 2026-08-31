import Foundation
import TeslatlasHubSDK

public struct TeslatlasCommandClient: Sendable {
  private let discovery: HubDiscoveryDocument
  private let selectedProtocolVersion: TeslatlasProtocolVersion
  private let authorization: any TeslatlasAuthorization
  private let transport: any TeslatlasHTTPTransport

  public init(
    discovery: HubDiscoveryDocument,
    selectedProtocolVersion: TeslatlasProtocolVersion,
    endpointTrustPolicy: HubEndpointTrustPolicy,
    authorization: any TeslatlasAuthorization,
    transport: any TeslatlasHTTPTransport = URLSessionTeslatlasTransport()
  ) throws {
    try endpointTrustPolicy.validate(discovery)
    self.discovery = discovery
    self.selectedProtocolVersion = selectedProtocolVersion
    self.authorization = authorization
    self.transport = transport
  }

  public func submit(
    _ requestBody: CommandRequest,
    idempotencyKey: UUID
  ) async throws -> CommandAcceptance {
    guard let capability = discovery.capability("commands.async"),
      let commands = capability.commands
    else {
      throw TeslatlasCommandError.capabilityUnavailable
    }
    guard
      let descriptor = commands.first(where: {
        $0.name == requestBody.command
      })
    else {
      throw TeslatlasCommandError.commandNotAdvertised(requestBody.command)
    }
    guard descriptor.commandClass == requestBody.commandClass.rawValue else {
      throw TeslatlasCommandError.commandClassMismatch(
        command: requestBody.command,
        expected: descriptor.commandClass,
        actual: requestBody.commandClass.rawValue
      )
    }
    if descriptor.confirmationRequired, requestBody.confirmation == nil {
      throw TeslatlasCommandError.confirmationRequired(requestBody.command)
    }

    let encoded = try JSONEncoder().encode(requestBody)
    guard encoded.count <= discovery.limits.maximumRequestBodyBytes else {
      throw TeslatlasSDKError.limitExceeded(
        name: "request_body_bytes",
        maximum: discovery.limits.maximumRequestBodyBytes,
        actual: encoded.count
      )
    }

    var request = URLRequest(
      url: discovery.endpoints.api.appendingPathComponent("commands")
    )
    request.httpMethod = "POST"
    request.httpBody = encoded
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    request.setValue(
      selectedProtocolVersion.description,
      forHTTPHeaderField: "Teslatlas-Protocol-Version"
    )
    request.setValue(
      idempotencyKey.uuidString,
      forHTTPHeaderField: "Idempotency-Key"
    )
    authorization.apply(to: &request)

    // Deliberately one attempt. Callers reconcile state before any reissue.
    let response = try await transport.send(request)
    guard response.statusCode == 202 else {
      throw try Self.decodeFailure(response)
    }
    guard
      response.header("Content-Type")?.lowercased()
        .hasPrefix("application/json") == true,
      response.header("Teslatlas-Protocol-Version")
        == selectedProtocolVersion.description,
      response.header("Cache-Control") != nil,
      response.header("Vary") != nil,
      let rawETag = response.header("ETag"),
      Self.isValidEntityTag(rawETag),
      let location = response.header("Location")
    else {
      throw TeslatlasCommandError.invalidResponse(
        "202 response is missing required protocol metadata"
      )
    }
    do {
      let job = try JSONDecoder().decode(CommandJob.self, from: response.body)
      return CommandAcceptance(
        job: job,
        entityTag: EntityTag(rawETag),
        location: location
      )
    } catch {
      throw TeslatlasCommandError.invalidResponse(
        "202 body does not match the command job schema"
      )
    }
  }

  private static func decodeFailure(_ response: TeslatlasHTTPResponse) throws
    -> Error
  {
    guard response.statusCode >= 400,
      response.header("Content-Type")?.lowercased()
        .hasPrefix("application/problem+json") == true
    else {
      return TeslatlasCommandError.invalidResponse(
        "unexpected HTTP \(response.statusCode)"
      )
    }
    do {
      let problem = try JSONDecoder().decode(
        TeslatlasProblemDetails.self,
        from: response.body
      )
      guard problem.status == response.statusCode else {
        return TeslatlasCommandError.invalidResponse(
          "problem status does not match HTTP status"
        )
      }
      return TeslatlasSDKError.problem(problem)
    } catch {
      return TeslatlasCommandError.invalidResponse("invalid problem details")
    }
  }

  private static func isValidEntityTag(_ rawValue: String) -> Bool {
    rawValue.range(
      of: #"^(?:W/)?\"[^\"]+\"$"#,
      options: .regularExpression
    ) != nil
  }
}
