// SPDX-License-Identifier: AGPL-3.0-only

import Darwin
import XCTest
@testable import Teslatlas_Hub

/// R-E1F6-22: tests must not write under the owner's home. Fixture roots come from
/// `HubTestScratch` (per-user Darwin temporary directory), and no test source may build a
/// path from the real home or the routed temporary directory.
final class HubTestScratchTests: XCTestCase {
    func testFixtureRootsLiveInTheDarwinUserTemporaryDirectoryNotTheHome() throws {
        let root = HubTestScratch.fixtureRoot("ScratchProbe")
        XCTAssertTrue(root.path.hasPrefix("/private/var/folders/"), root.path)
        XCTAssertEqual(root.deletingLastPathComponent().lastPathComponent, "teslatlas-hub-tests")
        XCTAssertTrue(root.lastPathComponent.hasPrefix("ScratchProbe-"), root.lastPathComponent)
        XCTAssertFalse(HubTestScratch.isUnderRealHome(root), root.path)
        XCTAssertNotEqual(HubTestScratch.fixtureRoot("ScratchProbe"), root, "every call is a new root")
        XCTAssertFalse(FileManager.default.fileExists(atPath: root.path), "the helper only names the root")
    }

    func testTheParentFolderIsPrivateAndItsAncestorsAreNotWritableByOthers() throws {
        let parent = HubTestScratch.directory
        var info = stat()
        XCTAssertEqual(lstat(parent.path, &info), 0)
        XCTAssertEqual(info.st_mode & S_IFMT, S_IFDIR, "a real folder, not a link")
        XCTAssertEqual(info.st_mode & 0o7777, 0o700)
        XCTAssertEqual(info.st_uid, getuid())
        // Every ancestor above the per-user temporary directory is a real folder that others cannot change.
        var ancestor = HubTestScratch.darwinTemporaryDirectory.deletingLastPathComponent()
        while ancestor.path != "/" {
            XCTAssertEqual(lstat(ancestor.path, &info), 0, ancestor.path)
            XCTAssertEqual(info.st_mode & S_IFMT, S_IFDIR, "\(ancestor.path) is not a link")
            XCTAssertEqual(info.st_mode & 0o022, 0, "\(ancestor.path) is writable by group or others")
            ancestor.deleteLastPathComponent()
        }
    }

    func testValidatedFixturesPassTheAppsOwnPrivatePathRulesOutsideTheHome() throws {
        let root = HubTestScratch.validatedFixtureRoot("ScratchValidated")
        XCTAssertTrue(root.path.hasPrefix("/Volumes/TeslatlasHubTests-"), root.path)
        XCTAssertFalse(HubTestScratch.isUnderRealHome(root), root.path)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false,
                                                attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: root) }
        // The development rules (strict ancestors, a 0700 leaf) and the LaunchAgent leaf rule both admit it.
        for (strict, policy) in [(false, HubDirectoryPolicy.privateLeaf), (true, .launchAgentsLeaf)] {
            let descriptor = try DevelopmentEventLog.validateDirectoryReadOnly(
                root, allowOwnedGroupWritableAncestors: strict, policy: policy)
            close(descriptor)
        }
        // The per-user Darwin directory cannot host such a fixture: that is why the volume exists.
        let darwin = HubTestScratch.fixtureRoot("ScratchValidated")
        try FileManager.default.createDirectory(at: darwin, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: darwin) }
        XCTAssertThrowsError(try DevelopmentEventLog.validateDirectoryReadOnly(darwin, allowOwnedGroupWritableAncestors: true))
    }

    func testStableBundleRootsHaveNoLinkAndNoExternalVolumePrefix() throws {
        let root = HubTestScratch.fixtureRoot("ScratchStable")
        XCTAssertFalse(HubTestScratch.isUnderRealHome(root), root.path)
        XCTAssertFalse(HubLaunchAgentPathPolicy.isUnstableLocation(root.path), root.path)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false,
                                                attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: root) }
        // Every component opens without following a link and is not world-writable.
        var descriptor = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)
        XCTAssertGreaterThanOrEqual(descriptor, 0)
        defer { close(descriptor) }
        for component in root.pathComponents.dropFirst() {
            let next = openat(descriptor, component, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW)
            XCTAssertGreaterThanOrEqual(next, 0, "\(component) of \(root.path) is a link or unreadable")
            guard next >= 0 else { return }
            close(descriptor); descriptor = next
            var entry = stat()
            XCTAssertEqual(fstat(descriptor, &entry), 0)
            XCTAssertTrue(HubLaunchAgentPathPolicy.admitsBundleAncestor(entry, ownerUID: getuid(), insideBundle: false), component)
        }
    }

    /// F6-1: the production test host's own log (and so every file it writes) stays in the folder the scripts clean.
    func testTheTestHostLogsInsideTheCleanedFolderAndLeavesNothingBesideIt() throws {
        let log = HubAppLog()   // the default location, as the production host and `HubAppLog.shared` use it
        let marker = "scratch.host.probe.\(UUID().uuidString)"
        log.record(marker, category: "test")
        let expected = HubTestScratch.directory
            .appendingPathComponent("host-\(getpid())", isDirectory: true)
            .appendingPathComponent("app.log")
        let text = try String(contentsOf: expected, encoding: .utf8)
        XCTAssertTrue(text.contains(marker), "the host's log is \(expected.path)")
        XCTAssertEqual(log.recentText().contains(marker), true)
        var info = stat()
        XCTAssertEqual(lstat(expected.deletingLastPathComponent().path, &info), 0)
        XCTAssertEqual(info.st_mode & 0o7777, 0o700, "the per-process folder is private")
        // Nothing of this process sits beside the cleaned folder (the old per-process spelling was a sibling).
        let siblings = try FileManager.default.contentsOfDirectory(atPath: HubTestScratch.darwinTemporaryDirectory.path)
        XCTAssertEqual(siblings.filter { $0 == "TeslatlasHubTests-\(getpid())" }, [])
    }

    func testPathsUnderTheRealHomeAreRecognisedWhateverTheirSpelling() {
        let home = HubTestScratch.realHomePath
        XCTAssertTrue(HubTestScratch.isUnderRealHome(URL(fileURLWithPath: home)))
        XCTAssertTrue(HubTestScratch.isUnderRealHome(URL(fileURLWithPath: home + "/dev/TeslatlasHubX-1")))
        XCTAssertTrue(HubTestScratch.isUnderRealHome(URL(fileURLWithPath: home + "/Library/../dev/x")))
        XCTAssertTrue(HubTestScratch.isUnderRealHome(URL(fileURLWithPath: home + "//dev/./x")))
        XCTAssertFalse(HubTestScratch.isUnderRealHome(URL(fileURLWithPath: home + "-other/x")))
        XCTAssertFalse(HubTestScratch.isUnderRealHome(URL(fileURLWithPath: "/private/var/folders/x")))
    }

    /// The source scan: the test sources in this folder (the staged copy that is being run).
    func testNoTestSourceBuildsAFixtureFromTheRealHomeOrTheRoutedTemporaryDirectory() throws {
        let folder = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let files = try FileManager.default.contentsOfDirectory(atPath: folder.path)
            .filter { $0.hasSuffix(".swift") }.sorted()
        XCTAssertGreaterThan(files.count, 10, "the scan found the test sources in \(folder.path)")
        // The real home is only ever compared with or refused: any line that names it and also creates,
        // writes, links, moves or removes something is an offence.
        let writeTokens = ["createDirectory", ".write(", "createFile", "removeItem", "setAttributes", "mkdir",
                           "createSymbolicLink", "moveItem", "copyItem", "trashItem", "appendingPathComponent"]
        let homeTokens = ["NSHomeDirectory", "homeDirectoryForCurrentUser", "expandingTildeInPath",
                          "environment[\"HOME\"]", "getpwuid", "NSHomeDirectoryForUser"]
        let tempTokens = [".temporaryDirectory", "NSTemporaryDirectory", "confstr("]
        var offences: [String] = []
        for file in files where file != "HubTestScratch.swift" && file != "HubTestScratchTests.swift" {
            let text = try String(contentsOf: folder.appendingPathComponent(file), encoding: .utf8)
            for (index, rawLine) in text.split(separator: "\n", omittingEmptySubsequences: false).enumerated() {
                let line = rawLine.trimmingCharacters(in: .whitespaces)
                if line.hasPrefix("//") { continue }
                // a test's own name may mention the home (testRealHomeDefaultIsNSHomeDirectory); its body may not
                for token in homeTokens where line.contains(token) && !line.hasPrefix("func test") {
                    offences.append("\(file):\(index + 1) uses \(token) (only HubTestScratch.realHomePath may name the home)")
                }
                if line.contains("realHomePath"), let write = writeTokens.first(where: { line.contains($0) }) {
                    offences.append("\(file):\(index + 1) writes below the real home (\(write))")
                }
                for token in tempTokens where line.contains(token) {
                    offences.append("\(file):\(index + 1) uses \(token) (fixtures come from HubTestScratch)")
                }
            }
        }
        XCTAssertEqual(offences, [], "R-E1F6-22: " + offences.joined(separator: "; "))
    }
}
