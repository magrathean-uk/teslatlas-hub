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
