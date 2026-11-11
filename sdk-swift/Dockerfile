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
# SwiftPM preserves resource ownership while copying bundles, so the unprivileged
# build user must own the admitted inputs even though every input remains read-only.
COPY --chown=swiftuser:swiftuser teslatlas-sdk-swift/ /workspace/teslatlas-sdk-swift/
COPY --chown=swiftuser:swiftuser external-four-library-consumer/ /workspace/external-four-library-consumer/
RUN chmod -R a-w /workspace/teslatlas-sdk-swift /workspace/external-four-library-consumer

ENV HOME=/home/swiftuser
USER swiftuser

CMD ["swift", "run", "--package-path", "/workspace/external-four-library-consumer", "--scratch-path", "/tmp/teslatlas-platform-gate-build", "ExternalFourLibraryConsumer"]
