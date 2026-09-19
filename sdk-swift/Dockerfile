# Official multi-architecture manifest (amd64 and arm64/v8):
# sha256:1ad73b8f2a2300c650da0949519418565661d802765b9a99435df22bc947e2b4
FROM swift:6.0.3-jammy@sha256:1ad73b8f2a2300c650da0949519418565661d802765b9a99435df22bc947e2b4

RUN apt-get update \
  && apt-get install --no-install-recommends -y ca-certificates libcurl4-openssl-dev libssl-dev python3 \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --create-home --user-group --uid 10001 --shell /usr/sbin/nologin swiftuser \
  && install -d -o swiftuser -g swiftuser /workspace/teslatlas-sdk-swift

WORKDIR /workspace/teslatlas-sdk-swift

# Keep the image input explicit. The matching .dockerignore is a deny-by-default
# allowlist, so a private file or a nested build directory cannot enter the
# build context and later be copied by an accidental broad COPY.
COPY --chown=swiftuser:swiftuser Package.swift VERSION LICENSE /workspace/teslatlas-sdk-swift/
COPY --chown=swiftuser:swiftuser Sources/ /workspace/teslatlas-sdk-swift/Sources/
COPY --chown=swiftuser:swiftuser Tests/ /workspace/teslatlas-sdk-swift/Tests/
COPY --chown=swiftuser:swiftuser Examples/TeslatlasHubSDKExample/ /workspace/teslatlas-sdk-swift/Examples/TeslatlasHubSDKExample/
COPY --chown=swiftuser:swiftuser Examples/CurrentHubConsumer/Package.swift /workspace/teslatlas-sdk-swift/Examples/CurrentHubConsumer/
COPY --chown=swiftuser:swiftuser Examples/CurrentHubConsumer/Sources/ /workspace/teslatlas-sdk-swift/Examples/CurrentHubConsumer/Sources/
COPY --chown=swiftuser:swiftuser Examples/CurrentHubConsumer/Tests/ /workspace/teslatlas-sdk-swift/Examples/CurrentHubConsumer/Tests/

USER swiftuser

CMD ["swift", "test", "--skip", "CurrentHubLiveTests", "--skip", "CurrentHubMatrixWorkerTests", "--skip", "CurrentHubAppleConsumerTests", "--skip", "LiveHubBlackBoxTests"]
