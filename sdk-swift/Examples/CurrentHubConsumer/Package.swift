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
        .product(name: "TesatlasHubSDK", package: "tesatlas-sdk-swift"),
        .product(name: "TesatlasCommands", package: "tesatlas-sdk-swift"),
        .product(name: "TesatlasHubV1Compatibility", package: "tesatlas-sdk-swift"),
        .product(name: "TesatlasCurrentHub", package: "tesatlas-sdk-swift"),
      ]
    ),
    .executableTarget(
      name: "CurrentHubConsumer",
      dependencies: [
        .product(name: "TesatlasCurrentHub", package: "tesatlas-sdk-swift")
      ]
    ),
    .testTarget(
      name: "CurrentHubConsumerTests",
      dependencies: [
        "CurrentHubConsumer",
        .product(name: "TesatlasCurrentHub", package: "tesatlas-sdk-swift")
      ]
    )
  ]
)
