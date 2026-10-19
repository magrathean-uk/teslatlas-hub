// SPDX-License-Identifier: AGPL-3.0-only

import CryptoKit
import Darwin
import Foundation
import XCTest
@testable import Teslatlas_Hub

/// Delete Hub and Data's filesystem half, against fabricated homes under
/// `<Darwin temp>/teslatlas-hub-tests/DataRemovalTests-<uuid>/home` (HubTestScratch). No test touches the real `~/Library`,
/// the real preferences domain, launchctl or anything under `/Volumes`.
final class HubDataRemovalTests: XCTestCase {
    private var roots: [URL] = []

    override func tearDown() {
        for root in roots {
            // Restore anything a test made unwritable, then remove the fixture.
            _ = try? FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o700)],
                                                       ofItemAtPath: root.path)
            try? FileManager.default.removeItem(at: root)
        }
        roots.removeAll()
        super.tearDown()
    }

    // MARK: - Full run

    func testFullRunDeletesTargetsInOrderWithSeams() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let outside = fixture.root.appendingPathComponent("outside/shared.txt")
        try FileManager.default.createDirectory(at: outside.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("outside content".utf8).write(to: outside)
        XCTAssertEqual(link(outside.path, fixture.d1.appendingPathComponent("data/shared.txt").path), 0)
        let seams = Seams(fixture: fixture)
        let remover = seams.remover()

        let plan = try remover.check()
        XCTAssertTrue(seams.events.isEmpty, "check() is read-only and calls no seam")
        let report = remover.delete(plan)

        for target in [fixture.d1, fixture.d2, fixture.d3, fixture.d4, fixture.d4Cookies, fixture.d5, fixture.d6] {
            XCTAssertFalse(FileManager.default.fileExists(atPath: target.path), target.lastPathComponent)
        }
        XCTAssertEqual(try String(contentsOf: outside, encoding: .utf8), "outside content",
                       "a second hard link outside the Hub folder keeps its content")
        XCTAssertEqual(report.deleted, ["D1", "D2", "D3", "D4", "D5", "D6", "D7", "D8"])
        XCTAssertTrue(report.kept.isEmpty)
        // Order: D1, log suspension, D2 ... D6, stale imports, preferences; the clearer
        // received exactly the app's domain.
        XCTAssertEqual(seams.events, ["suspend[D1 gone, D2 present]", "stale[D6 gone]",
                                      "preferences[eu.teslatlas.hub.app]"])
    }

    func testNeighboursStayByteIdentical() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let before = try fixture.snapshot(of: fixture.neighbours)
        XCTAssertGreaterThanOrEqual(before.count, fixture.neighbours.count)
        let seams = Seams(fixture: fixture)
        let remover = seams.remover()
        _ = remover.delete(try remover.check())
        let after = try fixture.snapshot(of: fixture.neighbours)
        XCTAssertEqual(before, after, "content, mode and inode of every neighbour must be unchanged")
        XCTAssertEqual(seams.preferencesDomains, ["eu.teslatlas.hub.app"],
                       "only the clearer seam acts on the preferences domain")
    }

    func testEmptyHomeSucceeds() throws {
        let fixture = try makeFixture(createLibrary: false)
        let seams = Seams(fixture: fixture)
        seams.staleCount = 0
        let remover = seams.remover()
        let plan = try remover.check()
        XCTAssertTrue(plan.entries.isEmpty)
        let report = remover.delete(plan)
        XCTAssertEqual(report.deleted, [])
        XCTAssertTrue(report.kept.isEmpty)

        try FileManager.default.createDirectory(at: fixture.library, withIntermediateDirectories: true)
        let emptyLibrary = remover.delete(try remover.check())
        XCTAssertEqual(emptyLibrary.deleted, [])
    }

    // MARK: - Refusals

    private func assertRefused(_ fixture: Fixture, containing expected: String? = nil,
                               notContaining unexpected: String? = nil,
                               file: StaticString = #filePath, line: UInt = #line) throws {
        let before = try fixture.snapshot(of: [fixture.root])
        let seams = Seams(fixture: fixture)
        let remover = seams.remover()
        XCTAssertThrowsError(try remover.check(), file: file, line: line) { error in
            XCTAssertTrue(error is HubDataRemovalRefusal, "\(error)", file: file, line: line)
            if let expected {
                XCTAssertTrue(error.localizedDescription.contains(expected),
                              "\(error.localizedDescription) should contain \(expected)", file: file, line: line)
            }
            if let unexpected {
                XCTAssertFalse(error.localizedDescription.contains(unexpected),
                               "\(error.localizedDescription) must not contain \(unexpected)", file: file, line: line)
            }
        }
        XCTAssertTrue(seams.events.isEmpty, "no seam may run on a refusal", file: file, line: line)
        XCTAssertEqual(try fixture.snapshot(of: [fixture.root]), before,
                       "the fixture must be byte-identical afterwards", file: file, line: line)
    }

    func testSymlinkInsideHubFolderIsRefused() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let canary = fixture.root.appendingPathComponent("canary/c.txt")
        try FileManager.default.createDirectory(at: canary.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("canary".utf8).write(to: canary)
        try FileManager.default.createSymbolicLink(at: fixture.d1.appendingPathComponent("data/escape"),
                                                   withDestinationURL: canary.deletingLastPathComponent())
        try assertRefused(fixture, containing: "is a link to another location")
        XCTAssertEqual(try String(contentsOf: canary, encoding: .utf8), "canary")
    }

    func testHubFolderLinkFileOrSharedIsRefused() throws {
        // D1 itself a symbolic link to the canary.
        var fixture = try makeFixture()
        try fixture.populate()
        let canary = fixture.root.appendingPathComponent("canary")
        try FileManager.default.createDirectory(at: canary, withIntermediateDirectories: true)
        try Data("canary".utf8).write(to: canary.appendingPathComponent("c.txt"))
        try FileManager.default.removeItem(at: fixture.d1)
        try FileManager.default.createSymbolicLink(at: fixture.d1, withDestinationURL: canary)
        try assertRefused(fixture, containing: "is a link to another location")

        // D1 a regular file.
        fixture = try makeFixture()
        try fixture.populate()
        try FileManager.default.removeItem(at: fixture.d1)
        try Data("not a folder".utf8).write(to: fixture.d1)
        try assertRefused(fixture, containing: "is not a folder")

        // D1 and D2 group- or world-writable.
        for writable in [\Fixture.d1, \Fixture.d2] {
            fixture = try makeFixture()
            try fixture.populate()
            chmod(fixture[keyPath: writable].path, 0o770)
            try assertRefused(fixture, containing: "is shared with other users")
        }
    }

    func testBackupGenerationIsRefused() throws {
        // At the top of D1, nested under data/, and the legacy marker.
        let layouts: [(String, [String])] = [
            ("", ["backup-v4.json", "BACKUP_COMPLETE"]),
            ("data/backups/gen1", ["backup-v4.json", "BACKUP_COMPLETE"]),
            ("data/old", ["backup-v3.json"]),
            ("data/partial", ["BACKUP_COMPLETE"])
        ]
        for (directory, names) in layouts {
            let fixture = try makeFixture()
            try fixture.populate()
            let target = fixture.d1.appendingPathComponent(directory)
            try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
            for name in names { try Data("{}".utf8).write(to: target.appendingPathComponent(name)) }
            try assertRefused(fixture, containing: "looks like a Hub backup. Move it out of the Hub folder first")
        }
    }

    func testRecoveryExportIsRefused() throws {
        let magic = Data(HubDataRemovalMarkers.recoveryMagic)
        for (size, refused) in [(84, true), (100, true), (16_532, true), (83, false), (16_533, false)] {
            let fixture = try makeFixture()
            try fixture.populate()
            var contents = magic
            contents.append(Data(repeating: 0x41, count: size - magic.count))
            try contents.write(to: fixture.d1.appendingPathComponent("data/recovery.bin"))
            if refused {
                try assertRefused(fixture, containing: "looks like an exported recovery file. Move it out of the Hub folder first")
            } else {
                XCTAssertNoThrow(try Seams(fixture: fixture).remover().check(), "size \(size) is outside the bounds")
            }
        }
        // The right size without the magic is an ordinary file.
        let fixture = try makeFixture()
        try fixture.populate()
        try Data(repeating: 0x42, count: 100).write(to: fixture.d1.appendingPathComponent("data/not-a-recovery.bin"))
        XCTAssertNoThrow(try Seams(fixture: fixture).remover().check())
    }

    func testExportedDriveIsRefused() throws {
        for name in ["drive.GPX", "trip.gpx", "Mixed.GpX"] {
            let fixture = try makeFixture()
            try fixture.populate()
            try Data("<gpx/>".utf8).write(to: fixture.d1.appendingPathComponent("data/\(name)"))
            try assertRefused(fixture, containing: "looks like an exported drive. Move it out of the Hub folder first")
        }
    }

    func testDepthBeyond128IsRefused() throws {
        let deep = try makeFixture()
        try deep.populate()
        var path = deep.d1.appendingPathComponent("data")
        for _ in 0..<129 { path.appendPathComponent("d") }
        try FileManager.default.createDirectory(at: path, withIntermediateDirectories: true)
        try assertRefused(deep, containing: "is nested too deeply to check")

        let allowed = try makeFixture()
        try allowed.populate()
        var ok = allowed.d1.appendingPathComponent("data")
        for _ in 0..<(HubDataRemovalMarkers.maximumDepth - 2) { ok.appendPathComponent("d") }
        try FileManager.default.createDirectory(at: ok, withIntermediateDirectories: true)
        XCTAssertNoThrow(try Seams(fixture: allowed).remover().check(), "a chain within the limit is admitted")
    }

    func testRefusesDataDirOutsideHubFolder() throws {
        let cases: [(String, (Fixture) -> String)] = [
            ("an external string", { _ in "/Volumes/4TB/dev/lab/teslatlas-v7/x" }),
            ("a similarly prefixed neighbour", {
                $0.home.path + "/Library/Application Support/Teslatlas Hub Backup/data"
            }),
            ("a lexical escape", { $0.d1.path + "/../Other" }),
            ("the home itself", { $0.home.path })
        ]
        for (label, path) in cases {
            let fixture = try makeFixture()
            try fixture.populate()
            try fixture.writeConfig("data_dir = \(HubController.tomlBasicString(path(fixture)))\n")
            try assertRefused(fixture, containing: "keeps its data outside the Hub folder")
            _ = label
        }
        // The refusal names the normalised path.
        let fixture = try makeFixture()
        try fixture.populate()
        try fixture.writeConfig("data_dir = \"\(fixture.d1.path)/../Other\"\n")
        let seams = Seams(fixture: fixture)
        XCTAssertThrowsError(try seams.remover().check()) { error in
            XCTAssertTrue(error.localizedDescription.contains(fixture.d1.deletingLastPathComponent().appendingPathComponent("Other").path))
        }
    }

    func testUnreadableOrUnsupportedDataDirIsRefused() throws {
        let unreadable = [
            "data_dir = \"data\"\n",
            "data_dir = \"\"\"\n/x\n\"\"\"\n",
            "bind = \"127.0.0.1:8080\"\n",
            "[collector]\ndata_dir = \"/inside\"\n",
            "data_dir = { path = \"/x\" }\n",
            "data_dir.sub = \"/x\"\n"
        ]
        for text in unreadable {
            let fixture = try makeFixture()
            try fixture.populate()
            try fixture.writeConfig(text)
            try assertRefused(fixture, containing: "cannot tell from the Hub configuration where the Hub keeps its data")
        }
        // A non-UTF-8 configuration.
        let fixture = try makeFixture()
        try fixture.populate()
        try Data([0xFF, 0xFE, 0x00, 0x80, 0x81]).write(to: fixture.d1.appendingPathComponent("config.toml"))
        chmod(fixture.d1.appendingPathComponent("config.toml").path, 0o600)
        try assertRefused(fixture, containing: "cannot tell from the Hub configuration")

        // Supported forms inside D1 are accepted.
        for text in [
            "data_dir = '\(populatedDataDir)'\n",
            "data_dir = \"\(populatedDataDir)\" # the data\n",
            "data_dir = '\(populatedDataDir)' # literal\nbind = \"x\"\n"
        ] {
            let accepted = try makeFixture()
            try accepted.populate()
            try accepted.writeConfig(text.replacingOccurrences(of: populatedDataDir, with: accepted.d1.path + "/data"))
            XCTAssertNoThrow(try Seams(fixture: accepted).remover().check(), text)
        }
    }

    private let populatedDataDir = "@DATA@"

    func testOtherBundleIdentifierIsRefused() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let seams = Seams(fixture: fixture)
        for identifier in ["eu.teslatlas.hub.app.tests", "eu.teslatlas.hub", "", "EU.TESLATLAS.HUB.APP"] {
            let remover = seams.remover(bundleIdentifier: identifier)
            XCTAssertThrowsError(try remover.check()) { error in
                XCTAssertTrue(error.localizedDescription.contains("not the Teslatlas Hub app (\(identifier))"))
            }
            XCTAssertThrowsError(try HubDataRemovalTargets(home: fixture.home, bundleIdentifier: identifier))
        }
        XCTAssertTrue(seams.events.isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.d1.path))
    }

    func testPolicyRefusesOtherDeviceOwnerAndDevices() {
        let owner: uid_t = 501
        func entry(_ type: mode_t, uid: uid_t = 501, device: dev_t = 7, links: nlink_t = 1) -> stat {
            var value = stat()
            value.st_mode = type | 0o600
            value.st_uid = uid
            value.st_dev = device
            value.st_nlink = links
            return value
        }
        func verdict(_ name: String, _ value: stat) -> HubDataRemovalVerdict {
            HubDataRemovalPolicy.admit(name: name, entry: value, rootDevice: 7, owner: owner)
        }
        XCTAssertEqual(verdict("a", entry(S_IFREG)), .admit)
        XCTAssertEqual(verdict("a", entry(S_IFDIR)), .admit)
        XCTAssertEqual(verdict("a", entry(S_IFLNK)), .refuse(.link))
        XCTAssertEqual(verdict("a", entry(S_IFREG, device: 8)), .refuse(.otherDisk))
        XCTAssertEqual(verdict("a", entry(S_IFREG, uid: 0)), .refuse(.otherOwner))
        XCTAssertEqual(verdict("a", entry(S_IFBLK)), .refuse(.deviceFile))
        XCTAssertEqual(verdict("a", entry(S_IFCHR)), .refuse(.deviceFile))
        XCTAssertEqual(verdict("a", entry(S_IFIFO)), .admit, "a FIFO name may be unlinked")
        XCTAssertEqual(verdict("a", entry(S_IFSOCK)), .admit, "a socket name may be unlinked")
        XCTAssertEqual(verdict("a", entry(S_IFREG, links: 2)), .admit, "a multi-link regular file is admitted")
        XCTAssertEqual(verdict("Drive.GPX", entry(S_IFREG)), .refuse(.exportedDrive))
        XCTAssertEqual(verdict("drive.gpx", entry(S_IFDIR)), .admit, "only a regular file named .gpx is an export")
    }

    // MARK: - Anchoring and re-checks

    func testDescriptorAnchoringKeepsDeletionOnOpenedDirectory() throws {
        for swapped in ["Application Support", "Teslatlas Hub/data"] {
            let fixture = try makeFixture()
            try fixture.populate()
            let canary = fixture.root.appendingPathComponent("canary")
            try FileManager.default.createDirectory(at: canary, withIntermediateDirectories: true)
            try Data("canary".utf8).write(to: canary.appendingPathComponent("c.txt"))
            let target = swapped == "Application Support" ? fixture.library.appendingPathComponent(swapped)
                : fixture.appSupport.appendingPathComponent(swapped)
            var armed = false
            var swaps = 0
            let seams = Seams(fixture: fixture)
            let remover = seams.remover(afterOpening: { opened in
                guard armed, swaps == 0, opened == target.path else { return }
                swaps += 1
                let aside = URL(fileURLWithPath: target.path + ".real")
                try? FileManager.default.moveItem(at: target, to: aside)
                try? FileManager.default.createSymbolicLink(at: target, withDestinationURL: canary)
            })
            let plan = try remover.check()
            armed = true
            let report = remover.delete(plan)
            XCTAssertEqual(swaps, 1, "the hook must have swapped \(swapped)")
            XCTAssertEqual(try String(contentsOf: canary.appendingPathComponent("c.txt"), encoding: .utf8), "canary",
                           "a swapped component must never redirect deletion to the canary (\(swapped))")
            XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: canary.path), ["c.txt"])
            if swapped == "Application Support" {
                XCTAssertFalse(FileManager.default.fileExists(
                    atPath: target.path + ".real/Teslatlas Hub"),
                    "the deletion stayed on the opened directory")
                XCTAssertTrue(report.deleted.contains("D1"))
            }
        }
    }

    func testMarkerAddedBetweenCheckAndDeleteIsKept() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let seams = Seams(fixture: fixture)
        let remover = seams.remover()
        let plan = try remover.check()
        let generation = fixture.d1.appendingPathComponent("data/gen")
        try FileManager.default.createDirectory(at: generation, withIntermediateDirectories: true)
        try Data("{}".utf8).write(to: generation.appendingPathComponent("backup-v4.json"))
        try Data("".utf8).write(to: generation.appendingPathComponent("BACKUP_COMPLETE"))
        try Data("keep me".utf8).write(to: generation.appendingPathComponent("rows.db"))

        let report = remover.delete(plan)

        XCTAssertTrue(FileManager.default.fileExists(atPath: generation.appendingPathComponent("rows.db").path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.d1.appendingPathComponent("data").path),
                      "the parents of a kept entry stay")
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.d1.appendingPathComponent("config.toml").path),
                       "the rest of D1 goes")
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.d1.appendingPathComponent("data/db.sqlite").path))
        XCTAssertTrue(report.kept.contains { $0.path.hasSuffix("data/gen") && $0.reason.contains("looks like a Hub backup") },
                      "\(report.kept)")
        XCTAssertFalse(report.deleted.contains("D1"), "D1 is not reported deleted while an entry remains")
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.d2.path), "independent targets continue")
    }

    func testFileCreatedDuringWalkIsHandledByOneRescan() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        var created = 0
        let seams = Seams(fixture: fixture)
        let remover = seams.remover(beforeRemoving: { path in
            // The first directory about to be removed gains a new file: ENOTEMPTY.
            guard created == 0, path.hasSuffix("/nested") else { return }
            created += 1
            let url = self.expandedHome(path, in: fixture).appendingPathComponent("late.tmp")
            try? Data("late".utf8).write(to: url)
        })
        let report = remover.delete(try remover.check())
        XCTAssertEqual(created, 1)
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.d1.path), "one rescan and one retry succeed")
        XCTAssertTrue(report.kept.isEmpty, "\(report.kept)")
        XCTAssertTrue(report.deleted.contains("D1"))
    }

    func testFileCreatedInRootDuringWalkIsHandledByOneRescan() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        var created = 0
        let seams = Seams(fixture: fixture)
        let remover = seams.remover(beforeRemoving: { path in
            // The root itself is about to be removed and gains a file: ENOTEMPTY on the root.
            guard created == 0, path.hasSuffix(fixture.d1.lastPathComponent) else { return }
            created += 1
            let url = self.expandedHome(path, in: fixture).appendingPathComponent("late-root.tmp")
            try? Data("late".utf8).write(to: url)
        })
        let report = remover.delete(try remover.check())
        XCTAssertEqual(created, 1)
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.d1.path), "the root gets one rescan and one retry")
        XCTAssertTrue(report.kept.isEmpty, "\(report.kept)")
        XCTAssertTrue(report.deleted.contains("D1"))
    }

    func testRootRescanThatKeepsAnEntryReportsThatEntryNotAStaleErrno() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        var created = 0
        let seams = Seams(fixture: fixture)
        let remover = seams.remover(beforeRemoving: { path in
            guard created == 0, path.hasSuffix(fixture.d1.lastPathComponent) else { return }
            created += 1
            let url = self.expandedHome(path, in: fixture).appendingPathComponent("late-link")
            try? FileManager.default.createSymbolicLink(at: url, withDestinationURL: URL(fileURLWithPath: "/nonexistent-target"))
        })
        let report = remover.delete(try remover.check())
        XCTAssertEqual(created, 1)
        XCTAssertEqual(report.kept.count, 1, "only the entry the rescan refused: \(report.kept)")
        XCTAssertTrue(report.kept.first?.path.hasSuffix("late-link") == true, "\(report.kept)")
        XCTAssertFalse(report.kept.contains { $0.reason.contains("not empty") }, "no stale errno entry: \(report.kept)")
        XCTAssertFalse(report.deleted.contains("D1"))
    }

    func testNestedRescanThatKeepsAnEntryReportsThatEntryNotAStaleErrno() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        var created = 0
        let seams = Seams(fixture: fixture)
        let remover = seams.remover(beforeRemoving: { path in
            guard created == 0, path.hasSuffix("/nested") else { return }
            created += 1
            let url = self.expandedHome(path, in: fixture).appendingPathComponent("late-link")
            try? FileManager.default.createSymbolicLink(at: url, withDestinationURL: URL(fileURLWithPath: "/nonexistent-target"))
        })
        let report = remover.delete(try remover.check())
        XCTAssertEqual(created, 1)
        XCTAssertEqual(report.kept.count, 1, "only the entry the rescan refused: \(report.kept)")
        XCTAssertTrue(report.kept.first?.path.hasSuffix("late-link") == true, "\(report.kept)")
        XCTAssertFalse(report.kept.contains { $0.reason.contains("not empty") }, "no stale errno entry: \(report.kept)")
    }

    // MARK: - Second fix round (R-E1F2-2 to R-E1F2-6)

    private func recoveryExport(size: Int = 100) -> Data {
        var contents = Data(HubDataRemovalMarkers.recoveryMagic)
        contents.append(Data(repeating: 0x41, count: size - contents.count))
        return contents
    }

    func testUnreadableRecoveryExportIsRefusedAtCheckNotTreatedAsOrdinaryData() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let export = fixture.d1.appendingPathComponent("data/recovery.bin")
        try recoveryExport().write(to: export)
        chmod(export.path, 0o000)
        try assertRefused(fixture, containing: "could not be checked")
        XCTAssertTrue(FileManager.default.fileExists(atPath: export.path))
    }

    func testRecoveryExportMadeUnreadableBetweenCheckAndDeleteIsKeptAndReported() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let export = fixture.d1.appendingPathComponent("data/recovery.bin")
        // Readable at check time: a recovery export within bounds is refused, so the check
        // sees an ordinary file of the same size without the magic ...
        try Data(repeating: 0x42, count: 100).write(to: export)
        let seams = Seams(fixture: fixture)
        let remover = seams.remover()
        let plan = try remover.check()
        // ... and by deletion it is a recovery export nobody can read.
        try recoveryExport().write(to: export)
        chmod(export.path, 0o000)
        let report = remover.delete(plan)
        XCTAssertTrue(FileManager.default.fileExists(atPath: export.path), "an unreadable export is never deleted")
        XCTAssertTrue(report.kept.contains { $0.path.hasSuffix("data/recovery.bin") && $0.reason.contains("could not be checked") },
                      "\(report.kept)")
        XCTAssertFalse(report.deleted.contains("D1"))
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.d2.path), "independent targets continue")
    }

    func testRootPermissionsChangedBetweenCheckAndDeleteAreRefusedAgain() throws {
        for shared in [\Fixture.d1, \Fixture.d2] {
            let fixture = try makeFixture()
            try fixture.populate()
            let seams = Seams(fixture: fixture)
            let remover = seams.remover()
            let plan = try remover.check()
            let target = fixture[keyPath: shared]
            chmod(target.path, 0o770)
            let report = remover.delete(plan)
            XCTAssertTrue(FileManager.default.fileExists(atPath: target.appendingPathComponent(
                target == fixture.d1 ? "config.toml" : "app.log").path),
                          "a root that became shared is left alone: \(target.lastPathComponent)")
            XCTAssertTrue(report.kept.contains { $0.path.hasSuffix(target.lastPathComponent) && $0.reason.contains("is shared with other users") },
                          "\(report.kept)")
            XCTAssertFalse(report.deleted.contains(target == fixture.d1 ? "D1" : "D2"))
            XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.d3.path), "independent targets continue")
            chmod(target.path, 0o700)
        }
    }

    func testInaccessibleChildRefusesBeforeAnyMutation() throws {
        // D2 can be listed but not searched: its children cannot be inspected.
        let fixture = try makeFixture()
        try fixture.populate()
        chmod(fixture.d2.path, 0o400)
        defer { chmod(fixture.d2.path, 0o700) }
        try assertRefused(fixture, containing: "could not be checked")
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.d1.path), "D1 is untouched too")
    }

    func testInaccessibleLibraryPathRefusesInsteadOfLookingEmpty() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        chmod(fixture.home.path, 0o600)
        defer { chmod(fixture.home.path, 0o700) }
        let seams = Seams(fixture: fixture)
        XCTAssertThrowsError(try seams.remover().check()) { error in
            XCTAssertTrue(error is HubDataRemovalRefusal, "\(error)")
            XCTAssertTrue(error.localizedDescription.contains("could not be checked"), error.localizedDescription)
        }
        XCTAssertTrue(seams.events.isEmpty)
    }

    func testLibraryReplacedBySymlinkBeforeDeletionKeepsEverythingAndSaysSo() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let seams = Seams(fixture: fixture)
        let remover = seams.remover()
        let plan = try remover.check()
        let canary = fixture.root.appendingPathComponent("canary")
        let aside = fixture.home.appendingPathComponent("Library.real")
        try FileManager.default.moveItem(at: fixture.library, to: aside)
        try FileManager.default.createSymbolicLink(at: fixture.library, withDestinationURL: canary)

        let report = remover.delete(plan)

        XCTAssertEqual(report.deleted, [], "nothing was deleted")
        let names = report.kept.map(\.path)
        for suffix in ["Application Support/Teslatlas Hub", "Logs/Teslatlas Hub", "Caches/eu.teslatlas.hub.app",
                       "HTTPStorages/eu.teslatlas.hub.app", "HTTPStorages/eu.teslatlas.hub.app.binarycookies",
                       "WebKit/eu.teslatlas.hub.app", "Saved Application State/eu.teslatlas.hub.app.savedState",
                       "Preferences/eu.teslatlas.hub.app.plist"] {
            XCTAssertTrue(names.contains { $0.hasSuffix(suffix) }, "\(suffix) is named among the retained targets: \(names)")
        }
        XCTAssertFalse(report.warnings.isEmpty, "the failure itself is reported")
        XCTAssertTrue(seams.preferencesDomains.isEmpty, "preferences are not cleared after an early failure")
        XCTAssertTrue(FileManager.default.fileExists(atPath: aside.appendingPathComponent("Application Support/Teslatlas Hub/config.toml").path))
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: canary.path), ["c.txt"])

        // The user is never told the data was removed.
        let alert = MainWindowController.deleteFinalMessage(report: report, olderInstallerPresent: false)
        XCTAssertNotEqual(alert.messageText, "Teslatlas Hub has been deleted")
        XCTAssertFalse(alert.informativeText.contains("were removed from this Mac"), alert.informativeText)
        XCTAssertTrue(alert.informativeText.contains("Application Support/Teslatlas Hub"), alert.informativeText)
        XCTAssertTrue(alert.informativeText.contains("still on this Mac"), alert.informativeText)
    }

    // MARK: - Third fix round

    private func assertEverythingRetained(_ report: HubDataRemovalReport, seams: Seams,
                                          file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertEqual(report.deleted, [], "nothing was deleted", file: file, line: line)
        let names = report.kept.map(\.path)
        for suffix in ["Application Support/Teslatlas Hub", "Logs/Teslatlas Hub", "Caches/eu.teslatlas.hub.app",
                       "HTTPStorages/eu.teslatlas.hub.app", "HTTPStorages/eu.teslatlas.hub.app.binarycookies",
                       "WebKit/eu.teslatlas.hub.app", "Saved Application State/eu.teslatlas.hub.app.savedState",
                       "Preferences/eu.teslatlas.hub.app.plist"] {
            XCTAssertTrue(names.contains { $0.hasSuffix(suffix) }, "\(suffix) is named among the retained targets: \(names)",
                          file: file, line: line)
        }
        XCTAssertFalse(report.warnings.isEmpty, "the failure itself is reported", file: file, line: line)
        XCTAssertEqual(seams.logged, report.warnings, "the failure is logged as well", file: file, line: line)
        XCTAssertTrue(seams.preferencesDomains.isEmpty, "preferences are not cleared after an early failure",
                      file: file, line: line)
        XCTAssertTrue(seams.events.isEmpty, "no seam ran: \(seams.events)", file: file, line: line)
        let alert = MainWindowController.deleteFinalMessage(report: report, olderInstallerPresent: false)
        XCTAssertTrue(alert.informativeText.contains("Details: "), "the failure is shown: \(alert.informativeText)",
                      file: file, line: line)
        XCTAssertNotEqual(alert.messageText, "Teslatlas Hub has been deleted", file: file, line: line)
        XCTAssertFalse(alert.informativeText.contains("were removed from this Mac"), alert.informativeText,
                       file: file, line: line)
        XCTAssertTrue(alert.informativeText.contains("still on this Mac"), alert.informativeText, file: file, line: line)
        XCTAssertFalse(alert.informativeText.contains("eu.teslatlas.hub.app.plist"),
                       "the preferences file is never an item to delete in Finder: \(alert.informativeText)",
                       file: file, line: line)
        XCTAssertTrue(alert.informativeText.contains("saved settings could not be cleared"), alert.informativeText,
                      file: file, line: line)
    }

    func testLibraryMissingBeforeDeletionKeepsEverythingAndSaysSo() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let seams = Seams(fixture: fixture)
        let remover = seams.remover()
        let plan = try remover.check()
        XCTAssertFalse(plan.entries.isEmpty)
        // Renamed away between the second check and the deletion: no link in its place.
        let aside = fixture.home.appendingPathComponent("Library.moved")
        try FileManager.default.moveItem(at: fixture.library, to: aside)

        let report = remover.delete(plan)

        assertEverythingRetained(report, seams: seams)
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.library.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: aside.appendingPathComponent("Application Support/Teslatlas Hub/config.toml").path),
                      "the data that was moved away is untouched")
        XCTAssertTrue(FileManager.default.fileExists(atPath: aside.appendingPathComponent("Preferences/eu.teslatlas.hub.app.plist").path))
    }

    func testLibraryMissingBeforeDeletionOfAnEmptyPlanStaysASuccess() throws {
        let fixture = try makeFixture()
        let seams = Seams(fixture: fixture)
        seams.staleCount = 0
        let remover = seams.remover()
        let plan = try remover.check()
        XCTAssertTrue(plan.entries.isEmpty)
        XCTAssertFalse(plan.preferencesPresent)
        try FileManager.default.removeItem(at: fixture.library)
        let report = remover.delete(plan)
        XCTAssertTrue(report.kept.isEmpty, "nothing was planned, so nothing is retained: \(report.kept)")
        XCTAssertTrue(report.warnings.isEmpty)
    }

    func testRootParentFolderMissingBeforeDeletionRetainsEverythingAndSaysSo() throws {
        // Any planned root's parent folder renamed away between the second check and the
        // deletion fails the whole data step: nothing is deleted, preferences are kept.
        for parent in ["Application Support", "Logs", "Caches", "HTTPStorages", "WebKit", "Saved Application State"] {
            let fixture = try makeFixture()
            try fixture.populate()
            let seams = Seams(fixture: fixture)
            let remover = seams.remover()
            let plan = try remover.check()
            let original = fixture.library.appendingPathComponent(parent)
            let aside = fixture.library.appendingPathComponent(parent + ".moved")
            try FileManager.default.moveItem(at: original, to: aside)

            let report = remover.delete(plan)

            assertEverythingRetained(report, seams: seams)
            XCTAssertTrue(report.warnings.contains { $0.contains("~/Library/" + parent) && $0.contains("changed while it was being deleted") },
                          "\(parent): \(report.warnings)")
            for target in [fixture.d1, fixture.d2, fixture.d3, fixture.d4, fixture.d4Cookies, fixture.d5, fixture.d6]
            where !target.path.hasPrefix(original.path + "/") {
                XCTAssertTrue(FileManager.default.fileExists(atPath: target.path),
                              "independent targets are not deleted either: \(target.lastPathComponent) after \(parent) went")
            }
            XCTAssertTrue(FileManager.default.fileExists(atPath: aside.path), "the moved folder is untouched: \(parent)")
            XCTAssertTrue(FileManager.default.fileExists(
                atPath: fixture.library.appendingPathComponent("Preferences/eu.teslatlas.hub.app.plist").path))
        }
    }

    func testRootParentFolderReplacedByAnotherFolderBeforeDeletionRetainsEverythingAndSaysSo() throws {
        // The parent folder renamed away and replaced by a different folder of the same name
        // (empty, or a copy of the old one) is not the folder that was checked: a whole-step
        // failure like a missing one, whatever the new folder holds.
        for parent in ["Application Support", "Logs", "Caches", "HTTPStorages", "WebKit", "Saved Application State"] {
            for copyBack in [false, true] {
                let label = "\(parent) \(copyBack ? "replaced by a copy" : "replaced by an empty folder")"
                let fixture = try makeFixture()
                try fixture.populate()
                let seams = Seams(fixture: fixture)
                let remover = seams.remover()
                let plan = try remover.check()
                let original = fixture.library.appendingPathComponent(parent)
                let aside = fixture.library.appendingPathComponent(parent + ".moved")
                try FileManager.default.moveItem(at: original, to: aside)
                if copyBack {
                    try FileManager.default.copyItem(at: aside, to: original)
                } else {
                    try FileManager.default.createDirectory(at: original, withIntermediateDirectories: false,
                                                            attributes: [.posixPermissions: 0o700])
                    try Data("canary".utf8).write(to: original.appendingPathComponent("canary.txt"))
                }

                let report = remover.delete(plan)

                assertEverythingRetained(report, seams: seams)
                XCTAssertTrue(report.warnings.contains { $0.contains("~/Library/" + parent) && $0.contains("changed while it was being deleted") },
                              "\(label): \(report.warnings)")
                for target in [fixture.d1, fixture.d2, fixture.d3, fixture.d4, fixture.d4Cookies, fixture.d5, fixture.d6] {
                    // Where the target is now: inside the moved-aside folder, or where it was.
                    let inside = target.path.hasPrefix(original.path + "/")
                    let now = inside ? aside.path + target.path.dropFirst(original.path.count) : target.path
                    XCTAssertTrue(FileManager.default.fileExists(atPath: String(now)),
                                  "nothing is deleted: \(target.lastPathComponent) after \(label)")
                    if copyBack, inside {
                        XCTAssertTrue(FileManager.default.fileExists(atPath: target.path),
                                      "the copy is untouched: \(target.lastPathComponent) after \(label)")
                    }
                }
                XCTAssertTrue(FileManager.default.fileExists(atPath: aside.path), "the moved folder is untouched: \(label)")
                if !copyBack {
                    XCTAssertTrue(FileManager.default.fileExists(atPath: original.appendingPathComponent("canary.txt").path),
                                  "the new folder is untouched: \(label)")
                }
            }
        }
    }

    func testRootParentFolderUnreadableBeforeDeletionRetainsEverythingAndSaysSo() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let seams = Seams(fixture: fixture)
        let remover = seams.remover()
        let plan = try remover.check()
        let caches = fixture.library.appendingPathComponent("Caches")
        chmod(caches.path, 0o000)
        defer { chmod(caches.path, 0o700) }

        let report = remover.delete(plan)

        assertEverythingRetained(report, seams: seams)
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.d1.path), "D1 is not deleted either")
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.d2.path), "D2 is not deleted either")
    }

    func testPlannedRootRenamedAwayBeforeDeletionIsKeptAndReportedNotDropped() throws {
        for (label, root) in [("D1 folder", \Fixture.d1), ("D4 cookies file", \Fixture.d4Cookies)] {
            let fixture = try makeFixture()
            try fixture.populate()
            let seams = Seams(fixture: fixture)
            let remover = seams.remover()
            let plan = try remover.check()
            let target = fixture[keyPath: root]
            let aside = target.deletingLastPathComponent().appendingPathComponent("renamed-away-\(label.prefix(2))")
            try FileManager.default.moveItem(at: target, to: aside)

            let report = remover.delete(plan)

            XCTAssertTrue(report.kept.contains { $0.path.hasSuffix(target.lastPathComponent) && $0.reason.contains("changed while it was being deleted") },
                          "\(label) is retained and named: \(report.kept)")
            if root == \Fixture.d1 { XCTAssertFalse(report.deleted.contains("D1"), label) }
            XCTAssertTrue(FileManager.default.fileExists(atPath: aside.path), "the moved data is untouched: \(label)")
            XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.d3.path), "independent targets continue: \(label)")
            let alert = MainWindowController.deleteFinalMessage(report: report, olderInstallerPresent: false)
            XCTAssertFalse(alert.informativeText.contains("were removed from this Mac"), alert.informativeText)
            XCTAssertTrue(alert.informativeText.contains(target.lastPathComponent), alert.informativeText)
        }
    }

    func testCheckTimeStatFailureOfARootSaysItCouldNotBeCheckedNotThatItChanged() throws {
        // Logs can be listed but not searched: D2 cannot be inspected at all.
        let fixture = try makeFixture()
        try fixture.populate()
        let logs = fixture.library.appendingPathComponent("Logs")
        chmod(logs.path, 0o400)
        defer { chmod(logs.path, 0o700) }
        try assertRefused(fixture, containing: "could not be checked", notContaining: "changed while")
    }

    func testCheckTimeOpenFailureOfARootSaysItCouldNotBeCheckedNotThatItChanged() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        chmod(fixture.d3.path, 0o000)
        defer { chmod(fixture.d3.path, 0o700) }
        try assertRefused(fixture, containing: "could not be checked", notContaining: "changed while")
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.d1.path), "D1 is untouched too")
    }

    func testCheckTimeOpenFailureOfAParentFolderSaysItCouldNotBeChecked() throws {
        // Logs cannot be opened at all: D2's parent folder cannot be inspected.
        let fixture = try makeFixture()
        try fixture.populate()
        let logs = fixture.library.appendingPathComponent("Logs")
        chmod(logs.path, 0o000)
        defer { chmod(logs.path, 0o700) }
        try assertRefused(fixture, containing: "could not be checked", notContaining: "is not a folder")
    }

    func testCheckTimeOpenFailureOfLibraryItselfSaysItCouldNotBeChecked() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        chmod(fixture.library.path, 0o000)
        defer { chmod(fixture.library.path, 0o700) }
        try assertRefused(fixture, containing: "could not be checked", notContaining: "is not a folder")
    }

    func testLibraryWritableByOthersKeepsItsSpecificReason() throws {
        for mode: mode_t in [0o770, 0o777, 0o720] {
            let fixture = try makeFixture()
            try fixture.populate()
            chmod(fixture.library.path, mode)
            defer { chmod(fixture.library.path, 0o700) }
            try assertRefused(fixture, containing: "~/Library is shared with other users",
                              notContaining: "could not be checked")
        }
    }

    func testLibraryOwnedByAnotherUserKeepsItsSpecificReason() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let before = try fixture.snapshot(of: [fixture.root])
        let seams = Seams(fixture: fixture)
        XCTAssertThrowsError(try seams.remover(owner: getuid() &+ 1).check()) { error in
            XCTAssertTrue(error is HubDataRemovalRefusal, "\(error)")
            XCTAssertTrue(error.localizedDescription.contains("~/Library belongs to another user"), error.localizedDescription)
            XCTAssertFalse(error.localizedDescription.contains("could not be checked"), error.localizedDescription)
        }
        XCTAssertTrue(seams.events.isEmpty)
        XCTAssertEqual(try fixture.snapshot(of: [fixture.root]), before)
    }

    func testLibraryThatIsALinkIsSaidToBeALink() throws {
        let fixture = try makeFixture(createLibrary: false)
        let real = fixture.home.appendingPathComponent("Library.real", isDirectory: true)
        try FileManager.default.createDirectory(at: real, withIntermediateDirectories: true)
        try FileManager.default.createSymbolicLink(at: fixture.library, withDestinationURL: real)
        try assertRefused(fixture, containing: "~/Library is a link to another location",
                          notContaining: "is not a folder")
    }

    func testParentThatIsAPlainFileIsSaidToBeNotAFolderNotALink() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let logs = fixture.library.appendingPathComponent("Logs")
        try FileManager.default.removeItem(at: logs)
        try Data("not a folder".utf8).write(to: logs)
        try assertRefused(fixture, containing: "~/Library/Logs is not a folder", notContaining: "link")
    }

    func testParentThatIsALinkIsSaidToBeALink() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let logs = fixture.library.appendingPathComponent("Logs")
        let real = fixture.home.appendingPathComponent("Logs.real", isDirectory: true)
        try FileManager.default.moveItem(at: logs, to: real)
        try FileManager.default.createSymbolicLink(at: logs, withDestinationURL: real)
        try assertRefused(fixture, containing: "~/Library/Logs is a link to another location")
    }

    func testLibraryThatIsNotAFolderIsStillSaidToBeNotAFolder() throws {
        let fixture = try makeFixture(createLibrary: false)
        try Data("not a folder".utf8).write(to: fixture.library)
        try assertRefused(fixture, containing: "~/Library is not a folder")
    }

    func testCheckTimeOpenFailureOfANestedFolderSaysItCouldNotBeCheckedNotThatItChanged() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let nested = fixture.d1.appendingPathComponent("data/nested")
        chmod(nested.path, 0o000)
        defer { chmod(nested.path, 0o700) }
        try assertRefused(fixture, containing: "could not be checked", notContaining: "changed while")
    }

    func testPreferencesThatCouldNotBeClearedAreRetainedNotReportedRemoved() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let seams = Seams(fixture: fixture)
        seams.clearSucceeds = false
        let remover = seams.remover()
        let report = remover.delete(try remover.check())

        XCTAssertEqual(seams.preferencesDomains, ["eu.teslatlas.hub.app"], "the clear was attempted")
        XCTAssertFalse(report.deleted.contains("D8"), "\(report.deleted)")
        XCTAssertEqual(report.deleted, ["D1", "D2", "D3", "D4", "D5", "D6", "D7"])
        XCTAssertTrue(report.kept.contains { $0.path.hasSuffix("Preferences/eu.teslatlas.hub.app.plist")
                        && $0.reason.contains("could not be cleared") && $0.settings },
                      "\(report.kept)")
        let alert = MainWindowController.deleteFinalMessage(report: report, olderInstallerPresent: false)
        XCTAssertEqual(alert.messageText, "Teslatlas Hub was not fully deleted")
        XCTAssertFalse(alert.informativeText.contains("were removed from this Mac"), alert.informativeText)
        XCTAssertTrue(alert.informativeText.contains("saved settings could not be cleared"), alert.informativeText)
        XCTAssertFalse(alert.informativeText.contains("Delete them in Finder"),
                       "the preferences file is never an item to delete by hand: \(alert.informativeText)")
        XCTAssertFalse(alert.informativeText.contains("eu.teslatlas.hub.app.plist"), alert.informativeText)
    }

    func testPreferencesClearFailureWithoutAFileIsAWarningNotAKeptItem() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        try FileManager.default.removeItem(at: fixture.library.appendingPathComponent("Preferences/eu.teslatlas.hub.app.plist"))
        let seams = Seams(fixture: fixture)
        seams.clearSucceeds = false
        let remover = seams.remover()
        let plan = try remover.check()
        XCTAssertFalse(plan.preferencesPresent)

        let report = remover.delete(plan)

        XCTAssertEqual(seams.preferencesDomains, ["eu.teslatlas.hub.app"], "the clear was still attempted")
        XCTAssertTrue(report.kept.isEmpty, "no preferences file existed, so none is kept: \(report.kept)")
        XCTAssertFalse(report.deleted.contains("D8"))
        XCTAssertTrue(report.warnings.contains { $0.contains("saved settings could not be cleared") }, "\(report.warnings)")
        XCTAssertEqual(seams.logged, report.warnings)
        let alert = MainWindowController.deleteFinalMessage(report: report, olderInstallerPresent: false)
        XCTAssertEqual(alert.messageText, "Teslatlas Hub was not fully deleted")
        XCTAssertTrue(alert.informativeText.contains("saved settings could not be cleared"), alert.informativeText)
        XCTAssertFalse(alert.informativeText.contains("Delete them in Finder"), alert.informativeText)
        XCTAssertFalse(alert.informativeText.contains(".plist"), alert.informativeText)
    }

    func testPreferencesFileClearedWithNoFileIsNotReportedAsD8() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        try FileManager.default.removeItem(at: fixture.library.appendingPathComponent("Preferences/eu.teslatlas.hub.app.plist"))
        let seams = Seams(fixture: fixture)
        let remover = seams.remover()
        let report = remover.delete(try remover.check())
        XCTAssertFalse(report.deleted.contains("D8"), "no file existed: \(report.deleted)")
        XCTAssertTrue(report.kept.isEmpty)
        XCTAssertTrue(report.warnings.isEmpty)
    }

    func testFinalMessageNeverAsksForThePreferencesFileToBeDeletedAlongsideOtherItems() {
        let report = HubDataRemovalReport(
            deleted: ["D1"],
            kept: [HubDataRemovalKept(path: "~/Library/Caches/eu.teslatlas.hub.app", reason: "is shared with other users"),
                   HubDataRemovalKept(path: "~/Library/Preferences/eu.teslatlas.hub.app.plist",
                                      reason: "could not be cleared", settings: true)],
            warnings: [])
        let alert = MainWindowController.deleteFinalMessage(report: report, olderInstallerPresent: false)
        XCTAssertTrue(alert.informativeText.contains(
            "These items could not be deleted and are still on this Mac: ~/Library/Caches/eu.teslatlas.hub.app (is shared with other users). Delete them in Finder."),
                      alert.informativeText)
        XCTAssertTrue(alert.informativeText.contains("saved settings could not be cleared"), alert.informativeText)
        XCTAssertFalse(alert.informativeText.contains(".plist"), alert.informativeText)
    }

    func testPreferencesDomainIsClearedOnlyWhenSynchronisedAndNoKeyIsLeft() {
        func run(sync: Bool, keys: [String]?) -> (Bool, [String]) {
            var events: [String] = []
            let result = HubProductionEffects.clearPreferencesDomain(
                "eu.teslatlas.hub.app",
                remove: { events.append("remove:\($0)") },
                synchronize: { events.append("sync:\($0)"); return sync },
                keysLeft: { events.append("keys:\($0)"); return keys })
            return (result, events)
        }
        let removeAndSync = ["remove:eu.teslatlas.hub.app", "sync:eu.teslatlas.hub.app"]
        let all = removeAndSync + ["keys:eu.teslatlas.hub.app"]
        XCTAssertTrue(run(sync: true, keys: nil).0, "no key list means an empty domain")
        XCTAssertEqual(run(sync: true, keys: nil).1, all)
        XCTAssertTrue(run(sync: true, keys: []).0)
        let leftover = run(sync: true, keys: ["onboardingCompleted"])
        XCTAssertFalse(leftover.0, "a key still in the domain after the synchronise is a failed clear")
        XCTAssertEqual(leftover.1, all)
        let unsynchronised = run(sync: false, keys: nil)
        XCTAssertFalse(unsynchronised.0, "a failed synchronise is a failed clear")
        XCTAssertEqual(unsynchronised.1, removeAndSync, "a failed synchronise is final: no re-read is trusted")
    }

    func testProductionPreferencesClearerClearsThroughTheSynchroniseAndTheReRead() {
        // The composition HubDataRemover.production uses: with no effect given, the clearer
        // runs clearPreferencesDomain, whose synchronise and re-read decide the result.
        let live: [String: String] = [:]
        func run(sync: Bool, keys: [String]?) -> (Bool, [String]) {
            var events: [String] = []
            let result = HubProductionEffects.preferencesClearer(
                environment: live,
                remove: { events.append("remove:\($0)") },
                synchronize: { events.append("sync:\($0)"); return sync },
                keysLeft: { events.append("keys:\($0)"); return keys })("d")
            return (result, events)
        }
        XCTAssertEqual(run(sync: true, keys: nil).1, ["remove:d", "sync:d", "keys:d"])
        XCTAssertTrue(run(sync: true, keys: nil).0)
        XCTAssertTrue(run(sync: true, keys: []).0)
        XCTAssertFalse(run(sync: true, keys: ["onboardingCompleted"]).0,
                       "a key left after the synchronise fails the production clear")
        XCTAssertFalse(run(sync: false, keys: nil).0, "a failed synchronise fails the production clear")
        // The test-host guard still comes first.
        var touched = false
        XCTAssertTrue(HubProductionEffects.preferencesClearer(
            remove: { _ in touched = true }, synchronize: { _ in touched = true; return false },
            keysLeft: { _ in touched = true; return ["x"] })("d"))
        XCTAssertFalse(touched, "a test host never reaches the preferences")
    }

    func testProductionRemoverPreferencesCompositionRequiresSynchroniseAndReRead() throws {
        // HubDataRemover.production itself, with recording preferences primitives and a
        // fixture filesystem: the composition under review, not a ready-made closure.
        func run(sync: Bool, keys: [String]?) throws -> (events: [String], report: HubDataRemovalReport) {
            let fixture = try makeFixture()
            try fixture.populate()
            var events: [String] = []
            let remover = HubDataRemover.production(
                home: fixture.home,
                bundleIdentifier: HubDataRemovalTargets.bundleIdentifier,
                effectsEnvironment: [:],
                preferences: HubPreferencesPrimitives(
                    remove: { events.append("remove:\($0)") },
                    synchronize: { events.append("sync:\($0)"); return sync },
                    keysLeft: { events.append("keys:\($0)"); return keys }),
                staleImportEffect: { .init() },
                suspendAppLog: {})
            let report = remover.delete(try remover.check())
            return (events, report)
        }
        let domain = HubDataRemovalTargets.bundleIdentifier

        let success = try run(sync: true, keys: nil)
        XCTAssertEqual(success.events, ["remove:\(domain)", "sync:\(domain)", "keys:\(domain)"],
                       "remove, then synchronise, then re-read")
        XCTAssertTrue(success.report.deleted.contains("D8"), "\(success.report)")
        XCTAssertTrue(success.report.kept.isEmpty, "\(success.report)")

        let emptyKeys = try run(sync: true, keys: [])
        XCTAssertTrue(emptyKeys.report.deleted.contains("D8"), "an empty re-read is a success")

        let failedSync = try run(sync: false, keys: nil)
        XCTAssertEqual(failedSync.events, ["remove:\(domain)", "sync:\(domain)"],
                       "a failed synchronise stops before the re-read")
        XCTAssertFalse(failedSync.report.deleted.contains("D8"))
        XCTAssertTrue(failedSync.report.kept.contains { $0.settings }, "the settings are reported as kept")

        let keysRemain = try run(sync: true, keys: ["onboardingCompleted"])
        XCTAssertEqual(keysRemain.events, ["remove:\(domain)", "sync:\(domain)", "keys:\(domain)"])
        XCTAssertFalse(keysRemain.report.deleted.contains("D8"), "a key left after the synchronise fails the clear")
        XCTAssertTrue(keysRemain.report.kept.contains { $0.settings })

        // The test-host guard still comes first when the real environment is used.
        var touched = false
        let fixture = try makeFixture()
        try fixture.populate()
        let guarded = HubDataRemover.production(
            home: fixture.home,
            bundleIdentifier: HubDataRemovalTargets.bundleIdentifier,
            preferences: HubPreferencesPrimitives(remove: { _ in touched = true },
                                                  synchronize: { _ in touched = true; return false },
                                                  keysLeft: { _ in touched = true; return ["x"] }),
            staleImportEffect: { .init() },
            suspendAppLog: {})
        _ = guarded.delete(try guarded.check())
        XCTAssertFalse(touched, "a test host never reaches the preferences")
    }

    func testProductionRemoverLogsItsWarningsThroughTheAppLog() throws {
        // HubDataRemover.production is the real composition: a warning it raises reaches
        // the app's log (the test host's own log file here), not a discarded closure.
        let fixture = try makeFixture()
        try fixture.populate()
        let remover = HubDataRemover.production(home: fixture.home)
        let plan = try remover.check()
        XCTAssertFalse(plan.entries.isEmpty)
        try FileManager.default.moveItem(at: fixture.library, to: fixture.home.appendingPathComponent("Library.moved"))
        let marker = "data_removal.warning"
        func count() -> Int { HubAppLog.shared.recentText().components(separatedBy: marker).count - 1 }
        let before = count()

        let report = remover.delete(plan)

        XCTAssertFalse(report.warnings.isEmpty, "the early failure is a warning: \(report)")
        XCTAssertEqual(count(), before + report.warnings.count,
                       "every warning reaches the app log through the production remover")
        XCTAssertEqual(report.deleted, [], "nothing ran against the moved Library")
    }

    func testPreferencesClearedSuccessfullyAreStillReportedAsD8() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let seams = Seams(fixture: fixture)
        let remover = seams.remover()
        let report = remover.delete(try remover.check())
        XCTAssertTrue(report.deleted.contains("D8"))
        XCTAssertTrue(report.kept.isEmpty, "\(report.kept)")
    }

    func testStaleImportFailuresAndScanFailureJoinTheReport() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let seams = Seams(fixture: fixture)
        seams.staleCount = 0
        seams.staleFailures = [.init(path: "/tmp/th-1234", reason: "Permission denied")]
        seams.staleScanFailure = .init(path: "/tmp", reason: "Operation not permitted")
        let remover = seams.remover()
        let report = remover.delete(try remover.check())
        XCTAssertFalse(report.deleted.contains("D7"))
        XCTAssertTrue(report.kept.contains { $0.path == "/tmp/th-1234" && $0.reason == "Permission denied" }, "\(report.kept)")
        XCTAssertFalse(report.kept.contains { $0.path == "/tmp" }, "a failed scan is not an item to delete: \(report.kept)")
        XCTAssertTrue(report.warnings.contains { $0.contains("Operation not permitted") }, "\(report.warnings)")
        XCTAssertEqual(seams.logged, report.warnings, "the warning is logged")
        let alert = MainWindowController.deleteFinalMessage(report: report, olderInstallerPresent: false)
        XCTAssertTrue(alert.informativeText.contains("/tmp/th-1234"), alert.informativeText)
        XCTAssertTrue(alert.informativeText.contains("Details: temporary import folders could not be checked (Operation not permitted)."),
                      alert.informativeText)
    }

    func testStaleImportScanFailureAloneIsNotTurnedIntoAnItemToDeleteInFinder() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let seams = Seams(fixture: fixture)
        seams.staleCount = 0
        seams.staleScanFailure = .init(path: "/tmp", reason: "Operation not permitted")
        let remover = seams.remover()
        let report = remover.delete(try remover.check())
        XCTAssertTrue(report.kept.isEmpty, "\(report.kept)")
        XCTAssertTrue(report.warnings.contains { $0.contains("Operation not permitted") }, "\(report.warnings)")
        XCTAssertEqual(seams.logged, report.warnings, "the warning is logged")
        let alert = MainWindowController.deleteFinalMessage(report: report, olderInstallerPresent: false)
        XCTAssertFalse(alert.informativeText.contains("/tmp"), "the user is never told to delete /tmp: \(alert.informativeText)")
        XCTAssertFalse(alert.informativeText.contains("Delete them in Finder"), alert.informativeText)
        // A step that could not be checked is never reported as a full removal.
        XCTAssertEqual(alert.messageText, "Teslatlas Hub was not fully deleted")
        XCTAssertFalse(alert.informativeText.contains("were removed from this Mac"), alert.informativeText)
        XCTAssertTrue(alert.informativeText.contains("Details: temporary import folders could not be checked (Operation not permitted)."),
                      alert.informativeText)
    }

    func testFinalEnableWarningAloneStaysAFullSuccess() {
        let report = HubDataRemovalReport(deleted: ["D1"], kept: [],
                                          warnings: [HubDataRemovalReport.finalEnableFailedWarning])
        let alert = MainWindowController.deleteFinalMessage(report: report, olderInstallerPresent: false)
        XCTAssertEqual(alert.messageText, "Teslatlas Hub has been deleted")
        XCTAssertFalse(alert.informativeText.contains("Details:"), alert.informativeText)
        XCTAssertTrue(alert.informativeText.contains("macOS still lists the background service as turned off."))
    }

    func testStaleImportCleanerReportsAnEligibleDirectoryItCouldNotRemove() throws {
        let manager = FileManager.default
        let root = HubTestScratch.directory.appendingPathComponent("teslatlas-hub-cleanup-report-\(UUID().uuidString)", isDirectory: true)
        let removable = root.appendingPathComponent("th-\(UUID().uuidString)", isDirectory: true)
        let stuck = root.appendingPathComponent("th-\(UUID().uuidString)", isDirectory: true)
        let locked = stuck.appendingPathComponent("locked", isDirectory: true)
        try manager.createDirectory(at: removable, withIntermediateDirectories: true)
        try manager.createDirectory(at: locked, withIntermediateDirectories: true)
        try Data("secret".utf8).write(to: locked.appendingPathComponent("ssh-password"))
        chmod(locked.path, 0o500)
        defer {
            chmod(locked.path, 0o700)
            try? manager.removeItem(at: root)
        }

        let result = TeslaMateServerImporter.cleanupStaleTemporaryDirectoriesReporting(in: root)

        XCTAssertEqual(result.removed, 1)
        XCTAssertEqual(result.failures.map { URL(fileURLWithPath: $0.path).resolvingSymlinksInPath().path },
                       [stuck.resolvingSymlinksInPath().path], "\(result)")
        XCTAssertFalse(result.failures.first?.reason.isEmpty ?? true)
        XCTAssertNil(result.scanFailure)
        XCTAssertTrue(manager.fileExists(atPath: stuck.path))
        XCTAssertFalse(manager.fileExists(atPath: removable.path))
        // The Int entry point keeps returning the count.
        XCTAssertEqual(TeslaMateServerImporter.cleanupStaleTemporaryDirectories(in: root), 0)
    }

    func testStaleImportCleanerReportsAScanFailure() {
        let missing = HubTestScratch.directory
            .appendingPathComponent("teslatlas-hub-missing-cleanup-report-\(UUID().uuidString)", isDirectory: true)
        let result = TeslaMateServerImporter.cleanupStaleTemporaryDirectoriesReporting(in: missing)
        XCTAssertEqual(result.removed, 0)
        XCTAssertEqual(result.scanFailure?.path, missing.path, "\(result)")
        XCTAssertFalse(result.scanFailure?.reason.isEmpty ?? true)
    }

    // MARK: - Guards (D-E16)

    func testRemoverRealHomeGuardThrowsBeforeAnyFileSystemCall() throws {
        let fixture = try makeFixture()
        try fixture.populate()
        let before = try fixture.snapshot(of: [fixture.root])
        var hookCalls = 0
        let seams = Seams(fixture: fixture)
        // The fixture is both identities, as an XCTest host: every filesystem method refuses.
        let remover = seams.remover(realHome: fixture.home,
                                    environment: ["XCTestConfigurationFilePath": "/nonexistent"],
                                    afterOpening: { _ in hookCalls += 1 },
                                    beforeRemoving: { _ in hookCalls += 1 })
        XCTAssertThrowsError(try remover.check()) { error in
            XCTAssertTrue(error.localizedDescription.contains("not deleted by a test host"))
        }
        let report = remover.delete(HubDataRemovalPlan(entries: [], preferencesPresent: true))
        XCTAssertEqual(report.deleted, [])
        XCTAssertFalse(report.warnings.isEmpty)
        XCTAssertEqual(hookCalls, 0, "the guard comes before any filesystem call")
        XCTAssertTrue(seams.events.isEmpty, "no seam may run for the real home")
        XCTAssertEqual(try fixture.snapshot(of: [fixture.root]), before)

        // Outside a test host the same identities are not refused (the guard is the only difference).
        let outside = seams.remover(realHome: fixture.home, environment: [:])
        XCTAssertNoThrow(try outside.check())
    }

    func testDefaultSeamsRefuseUnderXCTest() {
        var invoked: [String] = []
        // The default process environment is an XCTest host: nothing may act.
        _ = HubProductionEffects.preferencesClearer(effect: { invoked.append("preferences:\($0)"); return false })("eu.teslatlas.hub.app")
        XCTAssertEqual(HubProductionEffects.staleImportCleaner(effect: { invoked.append("stale"); return .init(removed: 3) })().removed, 0)
        HubProductionEffects.appReveal(effect: { invoked.append("reveal:\($0.path)") })(Bundle.main.bundleURL)
        HubProductionEffects.terminator(effect: { invoked.append("terminate") })()
        HubProductionEffects.terminationReply(effect: { invoked.append("reply:\($0)") })(true)
        XCTAssertEqual(invoked, [], "a test host must never act on its real preferences, Finder or termination")

        // Outside a test host the same compositions act (the guard is the only difference).
        let live: [String: String] = [:]
        XCTAssertTrue(HubProductionEffects.preferencesClearer(environment: live, effect: { invoked.append("preferences:\($0)"); return true })("d"))
        XCTAssertFalse(HubProductionEffects.preferencesClearer(environment: live, effect: { _ in false })("d"),
                       "a failed clear is reported to the remover, not swallowed")
        XCTAssertEqual(HubProductionEffects.staleImportCleaner(environment: live, effect: { invoked.append("stale"); return .init(removed: 3) })().removed, 3)
        HubProductionEffects.appReveal(environment: live, effect: { invoked.append("reveal:\($0.lastPathComponent)") })(URL(fileURLWithPath: "/x/App.app"))
        HubProductionEffects.terminator(environment: live, effect: { invoked.append("terminate") })()
        HubProductionEffects.terminationReply(environment: live, effect: { invoked.append("reply:\($0)") })(true)
        XCTAssertEqual(invoked, ["preferences:d", "stale", "reveal:App.app", "terminate", "reply:true"])
    }

    // MARK: - Fixtures

    private func expandedHome(_ displayed: String, in fixture: Fixture) -> URL {
        displayed.hasPrefix("~") ? URL(fileURLWithPath: fixture.home.path + displayed.dropFirst()) : URL(fileURLWithPath: displayed)
    }

    private func makeFixture(createLibrary: Bool = true) throws -> Fixture {
        let root = HubTestScratch.validatedFixtureRoot("DataRemovalTests")
        roots.append(root)
        let home = root.appendingPathComponent("home", isDirectory: true)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        for directory in [root, home] {
            try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o700)],
                                                  ofItemAtPath: directory.path)
        }
        let fixture = Fixture(root: root, home: home)
        if createLibrary {
            try FileManager.default.createDirectory(at: fixture.library, withIntermediateDirectories: true)
        }
        return fixture
    }

    /// Recording seams: each call also records what the filesystem looked like at that moment.
    private final class Seams {
        let fixture: Fixture
        private(set) var events: [String] = []
        private(set) var preferencesDomains: [String] = []
        private(set) var logged: [String] = []
        var staleCount = 1
        var staleFailures: [TeslaMateServerImporter.StaleTemporaryCleanup.Failure] = []
        var staleScanFailure: TeslaMateServerImporter.StaleTemporaryCleanup.Failure?
        var clearSucceeds = true

        init(fixture: Fixture) { self.fixture = fixture }

        func remover(bundleIdentifier: String = HubDataRemovalTargets.bundleIdentifier,
                     realHome: URL = URL(fileURLWithPath: "/nonexistent-real-home"),
                     environment: [String: String] = [:],
                     owner: uid_t = getuid(),
                     afterOpening: ((String) -> Void)? = nil,
                     beforeRemoving: ((String) -> Void)? = nil) -> HubDataRemover {
            let fixture = self.fixture
            return HubDataRemover(
                home: fixture.home,
                bundleIdentifier: bundleIdentifier,
                suspendAppLog: { [self] in
                    let d1 = FileManager.default.fileExists(atPath: fixture.d1.path)
                    let d2 = FileManager.default.fileExists(atPath: fixture.d2.path)
                    events.append("suspend[D1 \(d1 ? "present" : "gone"), D2 \(d2 ? "present" : "gone")]")
                },
                cleanStaleImports: { [self] in
                    let d6 = FileManager.default.fileExists(atPath: fixture.d6.path)
                    events.append("stale[D6 \(d6 ? "present" : "gone")]")
                    return .init(removed: staleCount, failures: staleFailures, scanFailure: staleScanFailure)
                },
                clearPreferences: { [self] domain in
                    preferencesDomains.append(domain)
                    events.append("preferences[\(domain)]")
                    return clearSucceeds
                },
                logWarning: { [self] in logged.append($0) },
                environment: environment,
                realHome: realHome,
                owner: owner,
                afterOpeningComponent: afterOpening,
                beforeRemovingDirectory: beforeRemoving
            )
        }
    }

    private struct Fixture {
        let root: URL
        let home: URL

        var library: URL { home.appendingPathComponent("Library", isDirectory: true) }
        var appSupport: URL { library.appendingPathComponent("Application Support", isDirectory: true) }
        var d1: URL { appSupport.appendingPathComponent("Teslatlas Hub", isDirectory: true) }
        var d2: URL { library.appendingPathComponent("Logs/Teslatlas Hub", isDirectory: true) }
        var d3: URL { library.appendingPathComponent("Caches/eu.teslatlas.hub.app", isDirectory: true) }
        var d4: URL { library.appendingPathComponent("HTTPStorages/eu.teslatlas.hub.app", isDirectory: true) }
        var d4Cookies: URL { library.appendingPathComponent("HTTPStorages/eu.teslatlas.hub.app.binarycookies") }
        var d5: URL { library.appendingPathComponent("WebKit/eu.teslatlas.hub.app", isDirectory: true) }
        var d6: URL { library.appendingPathComponent("Saved Application State/eu.teslatlas.hub.app.savedState", isDirectory: true) }

        var neighbours: [URL] {
            [appSupport.appendingPathComponent("Teslatlas Hub Backup"),
             appSupport.appendingPathComponent("Other"),
             library.appendingPathComponent("Logs/Other"),
             library.appendingPathComponent("Caches/eu.teslatlas.hub.app.other"),
             library.appendingPathComponent("Preferences/eu.teslatlas.hub.app.plist"),
             library.appendingPathComponent("LaunchAgents/other.plist"),
             root.appendingPathComponent("canary")]
        }

        func populate() throws {
            let manager = FileManager.default
            func write(_ url: URL, _ text: String = "content", mode: Int = 0o600) throws {
                try manager.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
                try Data(text.utf8).write(to: url)
                try manager.setAttributes([.posixPermissions: NSNumber(value: mode)], ofItemAtPath: url.path)
            }
            for directory in [d1, d2, d3, d4, d5, d6] {
                try manager.createDirectory(at: directory, withIntermediateDirectories: true)
            }
            try manager.setAttributes([.posixPermissions: NSNumber(value: 0o700)], ofItemAtPath: d1.path)
            try manager.setAttributes([.posixPermissions: NSNumber(value: 0o700)], ofItemAtPath: d2.path)
            try write(d1.appendingPathComponent("config.toml"), "data_dir = \(HubController.tomlBasicString(d1.path + "/data"))\n")
            try write(d1.appendingPathComponent("data/db.sqlite"), "database")
            try write(d1.appendingPathComponent("data/nested/deep/file.txt"), "deep")
            try write(d1.appendingPathComponent("data/nested/other.txt"), "other")
            try write(d2.appendingPathComponent("app.log"), "log")
            try write(d2.appendingPathComponent("hub.out.log"), "out")
            try write(d3.appendingPathComponent("cache.bin"), "cache")
            try write(d4.appendingPathComponent("network.bin"), "net")
            try write(d4Cookies, "cookies")
            try write(d5.appendingPathComponent("web/data.bin"), "web")
            try write(d6.appendingPathComponent("windows.data"), "state")
            // Neighbours that must never be touched.
            try write(appSupport.appendingPathComponent("Teslatlas Hub Backup/data/b.txt"), "backup")
            try write(appSupport.appendingPathComponent("Other/o.txt"), "other app")
            try write(library.appendingPathComponent("Logs/Other/x.log"), "other log")
            try write(library.appendingPathComponent("Caches/eu.teslatlas.hub.app.other/y.bin"), "other cache")
            try write(library.appendingPathComponent("Preferences/eu.teslatlas.hub.app.plist"), "plist")
            try write(library.appendingPathComponent("LaunchAgents/other.plist"), "agent")
            try write(root.appendingPathComponent("canary/c.txt"), "canary")
        }

        func writeConfig(_ text: String) throws {
            let url = d1.appendingPathComponent("config.toml")
            try Data(text.utf8).write(to: url)
            chmod(url.path, 0o600)
        }

        /// path -> (type, mode, inode, content digest) for everything below each URL.
        func snapshot(of urls: [URL]) throws -> [String: String] {
            var result: [String: String] = [:]
            func visit(_ url: URL) throws {
                var info = stat()
                guard lstat(url.path, &info) == 0 else { return }
                var description = "\(info.st_mode & S_IFMT)|\(info.st_mode & 0o7777)|\(info.st_ino)"
                if info.st_mode & S_IFMT == S_IFREG {
                    description += "|" + ((try? Data(contentsOf: url)).map {
                        SHA256.hash(data: $0).map { String(format: "%02x", $0) }.joined()
                    } ?? "unreadable")
                } else if info.st_mode & S_IFMT == S_IFLNK {
                    description += "|" + ((try? FileManager.default.destinationOfSymbolicLink(atPath: url.path)) ?? "?")
                }
                result[url.path] = description
                if info.st_mode & S_IFMT == S_IFDIR {
                    for name in ((try? FileManager.default.contentsOfDirectory(atPath: url.path)) ?? []).sorted() {
                        try visit(url.appendingPathComponent(name))
                    }
                }
            }
            for url in urls { try visit(url) }
            return result
        }
    }
}
