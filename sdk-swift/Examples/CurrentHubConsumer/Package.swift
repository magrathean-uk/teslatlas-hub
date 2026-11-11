// swift-tools-version: 6.0

import PackageDescription

let package = Package(
  name: "CurrentHubConsumer",
  platforms: [.macOS(.v14)],
  dependencies: [
    .package(path: "../.."),
  ],
  targets: [
    .executableTarget(
      name: "FourLibraryContractConsumer",
      dependencies: [
        .product(name: "TeslatlasHubSDK", package: "teslatlas-sdk-swift"),
        .product(name: "TeslatlasCommands", package: "teslatlas-sdk-swift"),
        .product(name: "TeslatlasHubV1Compatibility", package: "teslatlas-sdk-swift"),
        .product(name: "TeslatlasCurrentHub", package: "teslatlas-sdk-swift"),
      ]
    ),
    .executableTarget(
      name: "CurrentHubConsumer",
      dependencies: [
        .product(name: "TeslatlasCurrentHub", package: "teslatlas-sdk-swift")
      ]
    ),
    .testTarget(
      name: "CurrentHubConsumerTests",
      dependencies: [
        "CurrentHubConsumer",
        .product(name: "TeslatlasCurrentHub", package: "teslatlas-sdk-swift")
      ]
    )
  ]
)
