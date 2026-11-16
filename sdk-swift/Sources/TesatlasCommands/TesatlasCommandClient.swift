import Foundation
import TesatlasHubSDK

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

public struct TesatlasCommandClient: Sendable {
  private let discovery: HubDiscoveryDocument
  private let selectedProtocolVersion: TesatlasProtocolVersion
  private let authorization: any TesatlasAuthorization
  private let transport: any TesatlasHTTPTransport

  public init(
    discovery: HubDiscoveryDocument,
    selectedProtocolVersion: TesatlasProtocolVersion,
    endpointTrustPolicy: HubEndpointTrustPolicy,
    authorization: any TesatlasAuthorization,
    transport: any TesatlasHTTPTransport = URLSessionTesatlasTransport()
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
      throw TesatlasCommandError.capabilityUnavailable
    }
    guard
      let descriptor = commands.first(where: {
        $0.name == requestBody.command
      })
    else {
      throw TesatlasCommandError.commandNotAdvertised(requestBody.command)
    }
    guard descriptor.commandClass == requestBody.commandClass.rawValue else {
      throw TesatlasCommandError.commandClassMismatch(
        command: requestBody.command,
        expected: descriptor.commandClass,
        actual: requestBody.commandClass.rawValue
      )
    }
    if descriptor.confirmationRequired, requestBody.confirmation == nil {
      throw TesatlasCommandError.confirmationRequired(requestBody.command)
    }

    let encoded = try JSONEncoder().encode(requestBody)
    guard encoded.count <= discovery.limits.maximumRequestBodyBytes else {
      throw TesatlasSDKError.limitExceeded(
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
      forHTTPHeaderField: "Tesatlas-Protocol-Version"
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
      response.header("Tesatlas-Protocol-Version")
        == selectedProtocolVersion.description,
      response.header("Cache-Control") != nil,
      response.header("Vary") != nil,
      let rawETag = response.header("ETag"),
      Self.isValidEntityTag(rawETag),
      let location = response.header("Location")
    else {
      throw TesatlasCommandError.invalidResponse(
        "202 response is missing required protocol metadata"
      )
    }
    do {
      try RichRepresentationValidator.validate(response.body, representation: "command.command_job")
      let job = try JSONDecoder().decode(CommandJob.self, from: response.body)
      guard job.vehicleID == requestBody.vehicleID,
        job.command == requestBody.command,
        job.commandClass == requestBody.commandClass
      else {
        throw TesatlasCommandError.invalidResponse("202 job does not match the submitted intent")
      }
      return CommandAcceptance(
        job: job,
        entityTag: EntityTag(rawETag),
        location: location
      )
    } catch let error as TesatlasCommandError {
      throw error
    } catch {
      throw TesatlasCommandError.invalidResponse(
        "202 body does not match the command job schema"
      )
    }
  }

  private static func decodeFailure(_ response: TesatlasHTTPResponse) throws
    -> Error
  {
    guard response.statusCode >= 400,
      response.header("Content-Type")?.lowercased()
        .hasPrefix("application/problem+json") == true
    else {
      return TesatlasCommandError.invalidResponse(
        "unexpected HTTP \(response.statusCode)"
      )
    }
    do {
      let problem = try JSONDecoder().decode(
        TesatlasProblemDetails.self,
        from: response.body
      )
      guard problem.status == response.statusCode else {
        return TesatlasCommandError.invalidResponse(
          "problem status does not match HTTP status"
        )
      }
      return TesatlasSDKError.problem(problem)
    } catch {
      return TesatlasCommandError.invalidResponse("invalid problem details")
    }
  }

  private static func isValidEntityTag(_ rawValue: String) -> Bool {
    rawValue.range(
      of: #"^(?:W/)?\"[^\"]+\"$"#,
      options: .regularExpression
    ) != nil
  }
}
