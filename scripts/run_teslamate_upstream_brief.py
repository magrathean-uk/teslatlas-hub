#!/usr/bin/env python3
"""Stable entry point for the TeslaMate upstream monitor."""

from __future__ import annotations

import teslamate_upstream_brief as monitor


def compose_report(**values: object) -> str:
    """Render Markdown without inheriting indentation from a multiline literal."""
    mention = str(values["mention"])
    timezone = values["timezone"]
    started = values["started"]
    previous = values["previous"]
    next_state = values["next_state"]
    upstream = str(values["upstream"])
    head_sha = str(values["head_sha"])
    compare_url = values["compare_url"]
    compare_status = str(values["compare_status"])
    commits = values["commits"]
    files = values["files"]
    releases = values["releases"]
    analysis = str(values["analysis"]).strip()

    title_date = started.astimezone(timezone)
    title = f"{title_date.day} {title_date.strftime('%B %Y')}"
    revision = f"[`{head_sha[:8]}`](https://github.com/{upstream}/commit/{head_sha})"
    if compare_url:
        revision = f"[upstream diff]({compare_url}) · {revision}"

    visible = (
        f"@{mention}\n\n"
        f"## TeslaMate → Teslatlas daily impact — {title}\n\n"
        f"**Window:** {monitor.human_datetime(previous.checked_at, timezone)} → "
        f"{monitor.human_datetime(started, timezone)}  \n"
        f"**Checked:** {len(commits)} commit(s), {len(files)} changed file(s), "
        f"{len(releases)} release(s) · {revision} · `{compare_status}`\n\n"
        f"{analysis}"
    )
    visible = monitor.cap_words(visible, monitor.MAX_VISIBLE_WORDS)
    return f"{visible}\n\n{monitor.state_marker(next_state)}"


def self_test() -> None:
    from datetime import datetime, timezone
    from zoneinfo import ZoneInfo

    now = datetime(2026, 8, 6, 9, 0, tzinfo=timezone.utc)
    state = monitor.State(
        sha="b2cf9b6976d179d5e44d6f77d2013c0785bc4834",
        checked_at=now,
        upstream="teslamate-org/teslamate",
    )
    rendered = compose_report(
        mention="magrathean-uk",
        timezone=ZoneInfo("Europe/London"),
        started=now,
        previous=state,
        next_state=state,
        upstream=state.upstream,
        head_sha=state.sha,
        compare_url=None,
        compare_status="identical",
        commits=[],
        files=[],
        releases=[],
        analysis="**Overall: NO ACTION**\n\n**Action** — None.",
    )
    visible = rendered.split("<!--", 1)[0]
    assert "\n        ##" not in visible
    assert "\n## TeslaMate" in visible
    assert len(visible.split()) <= monitor.MAX_VISIBLE_WORDS


if __name__ == "__main__":
    self_test()
    monitor.compose_report = compose_report
    raise SystemExit(monitor.main())
