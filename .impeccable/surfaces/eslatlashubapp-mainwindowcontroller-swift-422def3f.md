---
version: 1
slug: "eslatlashubapp-mainwindowcontroller-swift-422def3f"
primary_target: "macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift"
related_targets: ["macos/TeslatlasHubApp/TeslatlasHubApp/HubNavigationBar.swift","macos/TeslatlasHubApp/TeslatlasHubApp/HubVehicleViews.swift"]
---

# Hub Mac workspace

## Scope and mode

Operate. Redesign the complete native Hub control experience: main workspace, Vehicles, onboarding, Diagnostics, Logs, Service Details, account/import entry points, menus, appearance, resize behaviour, and accessibility. Preserve service behaviour and factual copy.

## Audience and job

The owner checks whether Hub is healthy, manages connected vehicles, imports data, and troubleshoots the private background service. Common actions should be one glance or one command away; technical depth should appear when requested.

## Direction contract

THESIS: A composed Mac operations workspace with four stable destinations, focused content, and contextual action; it refuses a sidebar-heavy administration shell and the repeated dashboard-card grid.

OWN-WORLD: A centered native toolbar control for Overview, Vehicles, Activity, and Settings; semantic status colour; compact SF typography; aligned label/value rows; native lists; and quiet content surfaces. Custom drawing is limited to product status, selection feedback, and identity.

STORY: The user sees Hub health first, moves predictably across the four main destinations, and opens Diagnostics, Logs, or Service Details inside the same window without losing orientation. Consequential controls remain clearly scoped to a selected vehicle or service.

FIRST VIEWPORT: A 960×730 resizable Mac window. The centered toolbar navigation keeps four 120×36 destinations on one baseline. Main information uses a focused 744-point column; onboarding uses a stable 640-point rail with a fixed action area. Import, account, appearance, and utility actions remain contextual or menu-accessible.

FORM: Four-destination native workspace, explicitly accepted by the owner after comparison with the earlier sidebar and compact-utility proposals; code-led AppKit implementation against the preview catalogue and approved composition sheet.

FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Constraints

The branch must preserve unrelated dirty work. AppKit and macOS 13 remain supported. Do not invent unavailable vehicle facts, weaken confirmations, or redesign the separate `app/` product.
