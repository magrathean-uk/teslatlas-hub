# Teslatlas Hub

<!-- impeccable:product-schema 1 -->

## Platform

macOS

## Users

The primary user runs a private Tesla telemetry Hub on their own Mac and needs to understand its health, manage connected vehicles, inspect activity, and recover from problems without becoming a service operator.

## Product Purpose

Teslatlas Hub collects and stores the user's Tesla telemetry locally, exposes it to trusted companion products, and gives the owner a clear native control surface for setup, status, vehicle commands, diagnostics, logs, import, and recovery.

## Positioning

Hub is a local-first, user-owned telemetry service whose native Mac control app operates the same private service and durable data used by the Teslatlas ecosystem.

## Operating Context

Hub runs as a background service. The control app is opened for setup, quick health checks, account and vehicle management, imports, troubleshooting, and maintenance. Logs, diagnostics, and service details stay inside the main window so users do not lose navigation context while resolving an issue.

## Capabilities and Constraints

- Preserve existing service lifecycle, account, import, vehicle-command, diagnostics, logs, recovery, and onboarding behaviour.
- Use native AppKit and remain compatible with the declared macOS 13 floor while adopting current macOS presentation through system components.
- Never imply unavailable vehicle state or allow UI polish to weaken confirmation, privacy, trust, filesystem, or service safeguards.
- The separate `app/` product is out of scope.

## Brand Commitments

Keep the Teslatlas Hub name, app icon, restrained status colours, private/local language, and precise operational tone. The owner requires a contemporary desktop Mac experience rather than an iOS-style card dashboard.

## Evidence on Hand

The source includes deterministic preview scenes for onboarding, dashboard, vehicles, diagnostics, logs, service details, and account management. Existing runtime acceptance covers the current Mac source-run product; a visual redesign must not broaden those functional claims.

## Product Principles

- Make Hub health legible at a glance.
- Let structure and native controls carry the interface.
- Keep powerful or consequential actions contextual and explicit.
- Preserve user ownership, privacy, and recoverability in every workflow.
- Reward frequent Mac use with keyboard access and adaptable windows.

## Accessibility & Inclusion

Support keyboard operation, system appearance, Reduce Motion, VoiceOver semantics, high-contrast system colours where available, and useful layouts from the minimum window size through large desktop windows.
