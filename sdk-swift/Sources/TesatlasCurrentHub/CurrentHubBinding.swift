import Foundation

public struct CurrentHubBinding: Equatable, Sendable {
  public static let approvedManifestSHA256 =
    "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"

  public let profileID: String
  public let manifestSHA256: String
  public let testedHubVersions: [String]
  public let maximumResponseBytes: Int
  public let requiredCapabilities: Set<String>
  public let optionalCapabilities: Set<String>

  public static func load() throws -> CurrentHubBinding {
    guard let manifestURL = Bundle.module.url(
      forResource: "SHA256SUMS",
      withExtension: nil,
      subdirectory: "Binding"
    ) else {
      throw CurrentHubBindingError.missingResource("Binding/SHA256SUMS")
    }
    let bindingRoot = manifestURL.deletingLastPathComponent()

    let manifest = try Data(contentsOf: manifestURL)
    let digest = CurrentHubSHA256.hexDigest(of: manifest)
    guard digest == approvedManifestSHA256 else {
      throw CurrentHubBindingError.manifestDigestMismatch(
        expected: approvedManifestSHA256,
        actual: digest
      )
    }
    guard let text = String(data: manifest, encoding: .utf8) else {
      throw CurrentHubBindingError.invalidManifest
    }

    var listed = Set<String>()
    for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
      if line.isEmpty { continue }
      let parts = line.split(separator: " ", omittingEmptySubsequences: true)
      guard parts.count == 2 else {
        throw CurrentHubBindingError.invalidManifest
      }
      let expected = String(parts[0])
      let name = String(parts[1])
      guard
        expected.count == 64,
        expected.utf8.allSatisfy({
          (48...57).contains($0) || (97...102).contains($0)
        }),
        !name.hasPrefix("/"),
        !name.split(separator: "/").contains(".."),
        listed.insert(name).inserted
      else {
        throw CurrentHubBindingError.invalidManifest
      }
      let memberURL = bindingRoot.appendingPathComponent(name)
      guard
        let member = try? Data(contentsOf: memberURL),
        CurrentHubSHA256.hexDigest(of: member) == expected
      else {
        throw CurrentHubBindingError.memberDigestMismatch(name)
      }
    }

    guard listed.contains("profile.json") else {
      throw CurrentHubBindingError.invalidManifest
    }
    let profileData = try Data(
      contentsOf: bindingRoot.appendingPathComponent("profile.json")
    )
    let profile = try JSONDecoder().decode(Profile.self, from: profileData)
    guard profile.profileID == "hub-http-v1@1.0.0" else {
      throw CurrentHubBindingError.invalidProfileIdentity(profile.profileID)
    }
    guard profile.maximumResponseBytes == 1_048_576 else {
      throw CurrentHubBindingError.invalidResponseBound(
        profile.maximumResponseBytes
      )
    }

    return CurrentHubBinding(
      profileID: profile.profileID,
      manifestSHA256: digest,
      testedHubVersions: ["2026.36.2"],
      maximumResponseBytes: profile.maximumResponseBytes,
      requiredCapabilities: ["query.vehicles", "query.current"],
      optionalCapabilities: ["query.drives", "sync.packs"]
    )
  }

  private struct Profile: Decodable {
    let profileID: String
    let maximumResponseBytes: Int

    enum CodingKeys: String, CodingKey {
      case profileID = "profile_id"
      case maximumResponseBytes = "max_response_bytes"
    }
  }
}

public enum CurrentHubBindingError: Error, Equatable, Sendable {
  case missingResource(String)
  case invalidManifest
  case manifestDigestMismatch(expected: String, actual: String)
  case memberDigestMismatch(String)
  case invalidProfileIdentity(String)
  case invalidResponseBound(Int)
}
