FROM ghcr.io/astral-sh/uv:0.12.23@sha256:61d393e44e249f2e4b526b6c7ddcecce245946826e608e11c93ad4f5bba55b21 AS uv

FROM python:3.14.0-slim-bookworm@sha256:d13fa0424035d290decef3d575cea23d1b7d5952cdf429df8f5542c71e961576

RUN apt-get update \
    && apt-get install --no-install-recommends --yes \
        build-essential \
        ca-certificates \
        curl \
        libssl-dev \
        openssl \
        procps \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin gate

WORKDIR /workspace/teslatlas-protocol

COPY --from=uv /uv /uvx /bin/
COPY pyproject.toml uv.lock ./
RUN uv sync --locked --group dev --no-install-project \
    && chown -R gate:gate .venv

COPY conformance ./conformance
COPY profiles ./profiles
COPY schemas ./schemas
COPY events ./events
COPY examples ./examples
COPY fixtures ./fixtures
COPY tests ./tests
COPY tools ./tools
COPY openapi ./openapi
COPY compatibility ./compatibility
COPY docs ./docs
COPY Dockerfile VERSION LICENSE NOTICE README.md ./

ENV PATH="/workspace/teslatlas-protocol/.venv/bin:${PATH}" \
    UV_OFFLINE=1
USER gate

CMD ["./tools/check"]
