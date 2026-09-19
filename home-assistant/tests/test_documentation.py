"""Regression checks for current and historical Home Assistant documentation."""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).parents[1]


def _read(relative: str) -> str:
    return (ROOT / relative).read_text()


def test_current_docs_bind_the_checked_profile_and_pending_boundary() -> None:
    corpus = "\n".join(
        _read(path)
        for path in (
            "README.md",
            "docs/architecture.md",
            "docs/protocol-readiness.md",
            "docs/product-versioning.md",
        )
    )

    assert "hub-http-v1@1.0.0" in corpus
    assert "candidate" in corpus.casefold()
    assert "not been published through HACS" in corpus
    assert "uses only the released public current-Hub HTTP API" not in corpus


def test_historical_designs_are_marked_and_point_to_current_guidance() -> None:
    for path in (
        "docs/plans/2026-08-30-foundation.md",
        "docs/superpowers/specs/2026-08-30-hacs-local-push-foundation-design.md",
    ):
        document = _read(path)
        assert "Status: Superseded" in document
        assert "2026-08-31-concurrent-foundation-merge-design.md" in document


def test_container_candidate_and_manual_install_boundaries_stay_explicit() -> None:
    compose = _read("compose.yaml")
    docker = _read("docs/docker.md")
    readme = _read("README.md")

    assert "ghcr.io/home-assistant/home-assistant:2026.8.3" in compose
    assert "stop_grace_period: 60s" in compose
    assert (
        "custom_components/teslatlas_hub:/config/custom_components/teslatlas_hub:ro"
        in compose
    )
    assert "selected Debian 13 ARM64 lane" in docker
    assert "not a general support" in docker
    assert "copy only" in readme
    assert "not a Supervisor add-on or a HACS" in readme
