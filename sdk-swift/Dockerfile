# Docker Official Image swift:6.0.3-jammy, linux/arm64/v8 child only.
# Parent OCI index: sha256:e2b0410500126d7f569d387b5817426cef5c38cc02dc494c3dc5edc8e10304d6
FROM --platform=linux/arm64/v8 swift:6.0.3-jammy@sha256:c84da0197afcc90ef90a64194d4d451be7c090a845bcbf632755f9c16334ba8f

RUN apt-get update \
  && apt-get install --no-install-recommends -y ca-certificates libcurl4-openssl-dev libssl-dev python3 \
  && rm -rf /var/lib/apt/lists/* \
  && useradd --create-home --user-group --uid 10001 --shell /usr/sbin/nologin swiftuser \
  && install -d -o root -g root -m 0755 /workspace

WORKDIR /workspace

# The context must be a verified source_handoff.py output, never the live repo.
# These are the separately checksummed canonical package and external consumer.
COPY teslatlas-sdk-swift/ /workspace/teslatlas-sdk-swift/
COPY external-four-library-consumer/ /workspace/external-four-library-consumer/

USER swiftuser

CMD ["swift", "build", "--package-path", "/workspace/external-four-library-consumer", "--scratch-path", "/tmp/teslatlas-platform-gate-build"]
