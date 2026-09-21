# Teslatlas Hub design system

<!-- impeccable:design-schema 1 -->

## Direction

Teslatlas Hub is a calm native Mac operations workspace. Four stable toolbar
destinations—Overview, Vehicles, Activity, and Settings—provide place without turning
the app into an administration sidebar. Health is legible first; technical depth is
progressively disclosed inside the same window.

The authoritative composition reference is
`/Users/bolyki/dev/teslatlas-lab/build/hub/target/design-review-20260921/approved-direction.png`.
Its companion `design-brief.md` governs behavior, state coverage, and motion. The
reference is directional rather than a literal pixel contract; native control
behavior, product truth, and the measured source tokens below take precedence.

## Structure

- Default window: 960×730 points, resizable, with native titlebar and toolbar.
- Four toolbar destinations use equal 120×36 point controls on one baseline.
- Main information uses a focused 744-point content column with 28-point side insets.
- Onboarding uses a stable 640-point content rail and a fixed action area.
- Diagnostics, Activity & Logs, and Service Details are embedded destinations with
  directional Back behavior; they are not separate utility windows.
- Vehicle commands remain grouped by function and preserve confirmation safeguards.

## Visual language

- San Francisco system typography: 22-point semibold headings, 14-point body and
  emphasis, 13-point actions, and 12-point labels/captions.
- Quiet semantic system surfaces in light and dark appearances. Accent is reserved
  for selection and primary action; green, orange, and red retain status meanings.
- Grouped information rows are 52 points high with 16-point horizontal insets,
  24-point icon bounds, and 18-point SF Symbols.
- Standard actions are 32 points high and at least 96 points wide. Navigation is a
  separate 36-point role. Corners use the shared 8/12/14-point control, card, and
  focused-surface radii.
- Prefer aligned rows and native lists over repeated icon-card grids. Every label,
  subtitle, symbol, and badge must contribute information.

## Interaction and motion

- Hover and press fills resolve over 140 ms; click feedback uses 120 ms.
- Same-level destination changes use 160 ms fades. Forward/back and disclosure
  changes use 220 ms directional motion.
- Motion is interruptible and starts from the presentation state. Resting screens do
  not loop decorative animation.
- Reduce Motion replaces directional travel with a short fade and removes animated
  layout movement without removing state feedback.
- Busy service operations disable navigation and Back until the operation owns a
  truthful completion or recovery state.

## Accessibility and resilience

- All routes remain available from native menus and keyboard shortcuts.
- Navigation exposes radio-button roles and selected values; icon-only controls carry
  labels, help, and visible focus rings.
- Light/dark appearance, empty, loading, error, stopped, degraded, disabled, long
  content, and minimum-window states are part of the shipping surface.
- Status never relies on color or animation alone. Consequential actions retain
  explicit confirmation and accurate pending/error language.

## Historical alternatives

The earlier sidebar workspace and compact utility sketches are exploration only.
Likewise, the older `hub/.impeccable/review/macos-*.png` sidebar captures do not define
the current target. The four-destination composition and the current-source renders
under `design-polish-20260921` supersede them.
