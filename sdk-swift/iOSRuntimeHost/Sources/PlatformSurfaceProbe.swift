import TesatlasCommands
import TesatlasCurrentHub
import TesatlasHubSDK
import TesatlasHubV1Compatibility

enum PlatformSurfaceProbe {
  static func exercisesAllPublicLibraries() -> Bool {
    guard TesatlasProtocolVersion("1.2.0")?.description == "1.2.0" else {
      return false
    }

    let commandClass: CommandClass = .climate
    let currentError: CurrentHubError = .capabilityUnavailable("platform-probe")
    let historicalError: HubV1Error = .capabilityUnavailable("platform-probe")

    return commandClass.rawValue == "climate"
      && currentError == .capabilityUnavailable("platform-probe")
      && historicalError == .capabilityUnavailable("platform-probe")
  }
}
