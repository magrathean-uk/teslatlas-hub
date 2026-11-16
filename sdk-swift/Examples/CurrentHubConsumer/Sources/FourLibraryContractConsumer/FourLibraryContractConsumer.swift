import TesatlasCommands
import TesatlasCurrentHub
import TesatlasHubSDK
import TesatlasHubV1Compatibility

@main
struct FourLibraryContractConsumer {
  static func main() {
    guard let richProtocol = TesatlasProtocolVersion("1.2.0") else {
      fatalError("the released rich Protocol version is invalid")
    }
    let commandClass = CommandClass.vehicleState
    let compatibilityQuery = HubV1DriveQuery(limit: 1)
    let currentHubQuery = CurrentHubDriveQuery(limit: 1)
    guard let compatibilityLimit = compatibilityQuery.limit,
      let currentHubLimit = currentHubQuery.limit
    else {
      fatalError("the public query initializers discarded their limits")
    }

    print("TesatlasHubSDK=rich-protocol-\(richProtocol)")
    print(
      "TesatlasCommands=rich-protocol-\(richProtocol) command-class=\(commandClass.rawValue)"
    )
    print(
      "TesatlasHubV1Compatibility=deployed-hub-v1.0.0 query-limit=\(compatibilityLimit)"
    )
    print(
      "TesatlasCurrentHub=hub-http-v1@1.0.0 query-limit=\(currentHubLimit)"
    )
  }
}
