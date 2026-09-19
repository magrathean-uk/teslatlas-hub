FROM ghcr.io/astral-sh/uv:0.12.9@sha256:8b940d3a9d65bed080436972241af2e21c84b5e8c9193f7014ed71479ee795ff AS uv

FROM python:3.13.13-slim-bookworm@sha256:355bfa66770995d7e9a0da4b3473b44d0cb451f6b56f5615ad9c39e3c4eca03f

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
COPY Dockerfile VERSION LICENSE README.md THIRD-PARTY-NOTICES.md ./

ENV PATH="/workspace/teslatlas-protocol/.venv/bin:${PATH}" \
    UV_OFFLINE=1
USER gate

CMD ["./tools/check"]
