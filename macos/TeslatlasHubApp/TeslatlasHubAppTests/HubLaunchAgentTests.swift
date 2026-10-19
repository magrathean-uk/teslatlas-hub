// SPDX-License-Identifier: AGPL-3.0-only

import Darwin
import XCTest
@testable import Teslatlas_Hub

/// Every test builds its own fixtures (0700, removed in `tearDown`) from `HubTestScratch`: the bundle in the
/// Darwin temporary folder, the writer's fixture home on the private test volume. Nothing touches
/// the real home or `~/Library`, the real preferences domain or launchd.
final class HubLaunchAgentTests: XCTestCase {
    private var root: URL!
    private var home: URL!
    private var bundle: URL!
    private var binary: URL!
    private var config: URL!
    private let xcTestEnvironment = ["XCTestConfigurationFilePath": "/dev/null"]
    private let neverTheRealHome = URL(fileURLWithPath: "/var/empty/teslatlas-never-the-real-home", isDirectory: true)

    override func setUpWithError() throws {
        // The bundle walk refuses a link and an external-volume path (the Darwin folder is neither), the
        // Library walk refuses everything below the Darwin folder: root and fixture home come from different places.
        root = HubTestScratch.fixtureRoot("LaunchAgentTests")
        home = HubTestScratch.validatedFixtureRoot("LaunchAgentHome")
        try makeDirectory(root, mode: 0o700)
        try makeDirectory(home, mode: 0o700)
        try makeDirectory(home.appendingPathComponent("Library"), mode: 0o700)
        (bundle, binary) = try makeBundle(named: "Teslatlas Hub.app")
        config = try makeConfig()
    }

    override func tearDownWithError() throws {
        for folder in [root, home].compactMap({ $0 }) {
            try? FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: folder.path)
            try? FileManager.default.removeItem(at: folder)
        }
    }

    // MARK: - Fixture helpers

    private func makeDirectory(_ url: URL, mode: Int16 = 0o755) throws {
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: mode)], ofItemAtPath: url.path)
    }

    @discardableResult
    private func makeBundle(named name: String) throws -> (URL, URL) {
        let app = root.appendingPathComponent(name, isDirectory: true)
        let resources = app.appendingPathComponent("Contents/Resources", isDirectory: true)
        try makeDirectory(app)
        try makeDirectory(app.appendingPathComponent("Contents"))
        try makeDirectory(resources)
        let binary = resources.appendingPathComponent("teslatlas-hub")
        try Data("#!/bin/sh\nexit 0\n".utf8).write(to: binary)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: binary.path)
        return (app, binary)
    }

    private func makeConfig() throws -> URL {
        let folder = home.appendingPathComponent("Library/Application Support/Teslatlas Hub", isDirectory: true)
        try makeDirectory(folder, mode: 0o700)
        let file = folder.appendingPathComponent("config.toml")
        try Data("data_dir = \"\(folder.path)/data\"\n".utf8).write(to: file)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
        return file
    }

    private func configuration(binary: URL? = nil, config: URL? = nil,
                               ownerUID: uid_t = getuid(), version: String = "9.9+1") -> HubLaunchAgentConfiguration {
        HubLaunchAgentConfiguration(home: home, binary: binary ?? self.binary, config: config ?? self.config,
                                    ownerUID: ownerUID, bundleVersion: version)
    }

    private func writer(_ configuration: HubLaunchAgentConfiguration? = nil,
                        realHome: URL? = nil,
                        environment: [String: String] = [:],
                        hook: ((String) -> Void)? = nil,
                        hooks: HubFileSystemHooks = .system,
                        temporaryName: (() -> String)? = nil) -> HubLaunchAgentWriter {
        let value = configuration ?? self.configuration()
        if let temporaryName {
            return HubLaunchAgentWriter(configuration: { value }, environment: environment,
                                        realHome: realHome ?? neverTheRealHome,
                                        afterOpeningComponent: hook, fileHooks: hooks,
                                        temporaryName: temporaryName)
        }
        return HubLaunchAgentWriter(configuration: { value }, environment: environment,
                                    realHome: realHome ?? neverTheRealHome,
                                    afterOpeningComponent: hook, fileHooks: hooks)
    }

    private var launchAgents: URL { home.appendingPathComponent("Library/LaunchAgents", isDirectory: true) }
    private var plist: URL { launchAgents.appendingPathComponent("com.teslatlas.hub.plist") }
    private var logs: URL { home.appendingPathComponent("Library/Logs/Teslatlas Hub", isDirectory: true) }

    private func plistDictionary(_ url: URL? = nil) throws -> [String: Any] {
        let data = try Data(contentsOf: url ?? plist)
        return try XCTUnwrap(try PropertyListSerialization.propertyList(from: data, options: [], format: nil) as? [String: Any])
    }

    private func status(_ url: URL) -> stat {
        var information = stat()
        XCTAssertEqual(lstat(url.path, &information), 0, url.path)
        return information
    }

    /// Path, type, mode, owner, inode, size and mtime of every entry below `url`.
    private func snapshot(_ url: URL) -> [String] {
        var lines: [String] = []
        guard let enumerator = FileManager.default.enumerator(atPath: url.path) else { return lines }
        for case let relative as String in enumerator {
            var information = stat()
            if lstat(url.appendingPathComponent(relative).path, &information) == 0 {
                lines.append("\(relative)|\(information.st_mode)|\(information.st_uid)|\(information.st_ino)|\(information.st_size)|\(information.st_mtimespec.tv_sec).\(information.st_mtimespec.tv_nsec)|\(information.st_nlink)")
            }
        }
        return lines.sorted()
    }

    private func temporaries() -> [String] {
        ((try? FileManager.default.contentsOfDirectory(atPath: launchAgents.path)) ?? [])
            .filter { $0.hasPrefix(".com.teslatlas.hub.plist.") && $0.hasSuffix(".tmp") }
    }

    private func appendByte(to url: URL) throws {
        let handle = try FileHandle(forWritingTo: url)
        try handle.seekToEnd()
        try handle.write(contentsOf: Data([0x0A]))
        try handle.close()
    }

    // MARK: - Property list

    func testPropertyListHasExactKeySetAndExplicitExitTimeOut() throws {
        _ = try writer().ensureInstalled(requireConfig: true)
        let parsed = try plistDictionary()
        XCTAssertEqual(Set(parsed.keys), [
            "Label", "ProgramArguments", "EnvironmentVariables", "StandardOutPath", "StandardErrorPath",
            "RunAtLoad", "KeepAlive", "ProcessType", "Umask", "ThrottleInterval", "ExitTimeOut"
        ])
        XCTAssertEqual(parsed["Label"] as? String, "com.teslatlas.hub")
        XCTAssertEqual(parsed["ProgramArguments"] as? [String], [binary.path, "--config", config.path, "serve"])
        let environment = try XCTUnwrap(parsed["EnvironmentVariables"] as? [String: String])
        XCTAssertEqual(Array(environment.keys), ["TESLATLAS_HUB_EXECUTABLE_IDENTITY"])
        XCTAssertFalse(environment.keys.contains { $0.hasPrefix("TESLATLAS_HUB_DEVELOPMENT") })
        XCTAssertEqual(parsed["StandardOutPath"] as? String, logs.appendingPathComponent("hub.out.log").path)
        XCTAssertEqual(parsed["StandardErrorPath"] as? String, logs.appendingPathComponent("hub.err.log").path)
        XCTAssertEqual(parsed["KeepAlive"] as? [String: Bool], ["SuccessfulExit": false])
        XCTAssertEqual(parsed["ProcessType"] as? String, "Background")
        for key in ["Umask": 63, "ThrottleInterval": 30, "ExitTimeOut": 270] {
            let number = try XCTUnwrap(parsed[key.key] as? NSNumber, key.key)
            XCTAssertNotEqual(CFGetTypeID(number), CFBooleanGetTypeID(), key.key)
            XCTAssertEqual(number.intValue, key.value, key.key)
        }
        XCTAssertEqual(try XCTUnwrap(parsed["RunAtLoad"] as? NSNumber).boolValue, true)
        XCTAssertNil(parsed["WorkingDirectory"])
        XCTAssertNil(parsed["AssociatedBundleIdentifiers"])
    }

    func testWrittenPlistLintsAndReadsBackExitTimeOut() throws {
        _ = try writer().ensureInstalled(requireConfig: true)
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/plutil")
        process.arguments = ["-lint", plist.path]
        let output = Pipe()
        process.standardOutput = output
        process.standardError = output
        try process.run()
        process.waitUntilExit()
        XCTAssertEqual(process.terminationStatus, 0)
        XCTAssertEqual((try plistDictionary()["ExitTimeOut"] as? NSNumber)?.intValue, 270)
    }

    func testWrittenPlistIsPrivateRegularSingleLinkWithNoTemporary() throws {
        _ = try writer().ensureInstalled(requireConfig: true)
        let information = status(plist)
        XCTAssertEqual(information.st_mode & S_IFMT, S_IFREG)
        XCTAssertEqual(information.st_uid, getuid())
        XCTAssertEqual(information.st_mode & 0o777, 0o600)
        XCTAssertEqual(information.st_nlink, 1)
        XCTAssertEqual(temporaries(), [])
        let directory = status(launchAgents)
        XCTAssertEqual(directory.st_mode & 0o777, 0o700)
    }

    func testInjectedWriteAndRenameFailuresLeaveNoTemporary() throws {
        _ = try writer().ensureInstalled(requireConfig: true)
        let original = try Data(contentsOf: plist)
        try appendByte(to: binary)
        var failingWrite = HubFileSystemHooks.system
        failingWrite.write = { _, _, _ in errno = EIO; return -1 }
        XCTAssertThrowsError(try writer(hooks: failingWrite).ensureInstalled(requireConfig: true))
        XCTAssertEqual(try Data(contentsOf: plist), original)
        XCTAssertEqual(temporaries(), [])
        var failingRename = HubFileSystemHooks.system
        failingRename.renameat = { _, _, _, _ in errno = EXDEV; return -1 }
        XCTAssertThrowsError(try writer(hooks: failingRename).ensureInstalled(requireConfig: true))
        XCTAssertEqual(try Data(contentsOf: plist), original)
        XCTAssertEqual(temporaries(), [])
    }

    func testTemporaryNameSymlinkIsNotFollowed() throws {
        try makeDirectory(launchAgents, mode: 0o700)
        let canary = root.appendingPathComponent("canary")
        try Data("canary".utf8).write(to: canary)
        let fixedName = ".com.teslatlas.hub.plist.fixed.tmp"
        try FileManager.default.createSymbolicLink(at: launchAgents.appendingPathComponent(fixedName),
                                                   withDestinationURL: canary)
        XCTAssertThrowsError(try writer(temporaryName: { fixedName }).ensureInstalled(requireConfig: true))
        XCTAssertEqual(try Data(contentsOf: canary), Data("canary".utf8))
        XCTAssertFalse(FileManager.default.fileExists(atPath: plist.path))
    }

    func testSecondEnsureIsUnchangedWithSameInodeAndMtime() throws {
        let subject = writer()
        XCTAssertEqual(try subject.ensureInstalled(requireConfig: true), .created)
        let first = status(plist)
        XCTAssertEqual(try subject.ensureInstalled(requireConfig: true), .unchanged)
        let second = status(plist)
        XCTAssertEqual(first.st_ino, second.st_ino)
        XCTAssertEqual(first.st_mtimespec.tv_sec, second.st_mtimespec.tv_sec)
        XCTAssertEqual(first.st_mtimespec.tv_nsec, second.st_mtimespec.tv_nsec)
    }

    func testChangedIdentityOrBinaryPathReplacesPlist() throws {
        XCTAssertEqual(try writer().ensureInstalled(requireConfig: true), .created)
        // A changed size and mtime.
        try appendByte(to: binary)
        XCTAssertEqual(try writer().ensureInstalled(requireConfig: true), .replaced)
        XCTAssertEqual(try writer().ensureInstalled(requireConfig: true), .unchanged)
        // Same path, same size, same mtime, new inode: only the inode differs.
        let before = status(binary)
        let copy = binary.deletingLastPathComponent().appendingPathComponent("copy")
        try FileManager.default.copyItem(at: binary, to: copy)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: copy.path)
        var times = [before.st_atimespec, before.st_mtimespec]
        XCTAssertEqual(utimensat(AT_FDCWD, copy.path, &times, 0), 0)
        XCTAssertEqual(rename(copy.path, binary.path), 0)
        let after = status(binary)
        XCTAssertNotEqual(before.st_ino, after.st_ino)
        XCTAssertEqual(before.st_size, after.st_size)
        XCTAssertEqual(before.st_mtimespec.tv_nsec, after.st_mtimespec.tv_nsec)
        XCTAssertEqual(try writer().ensureInstalled(requireConfig: true), .replaced)
        // A different binary path.
        let (_, moved) = try makeBundle(named: "Moved Hub.app")
        XCTAssertEqual(try writer(configuration(binary: moved)).ensureInstalled(requireConfig: true), .replaced)
        XCTAssertEqual(try plistDictionary()["ProgramArguments"] as? [String],
                       [moved.path, "--config", config.path, "serve"])
    }

    func testPkgEraAndRustInstallShapesAreReplaced() throws {
        try makeDirectory(launchAgents, mode: 0o755)
        let shapes: [[String: Any]] = [
            [
                "Label": "com.teslatlas.hub",
                "ProgramArguments": ["/Library/Application Support/Teslatlas Hub/libexec/run-hub-service.sh",
                                     "--stdout-log", logs.appendingPathComponent("hub.out.log").path,
                                     "--stderr-log", logs.appendingPathComponent("hub.err.log").path],
                "RunAtLoad": true, "KeepAlive": ["SuccessfulExit": false]
            ],
            [
                "Label": "com.teslatlas.hub",
                "ProgramArguments": [home.appendingPathComponent("Library/Application Support/Teslatlas Hub/data/bin/teslatlas-hub").path,
                                     "--config", config.path, "serve"],
                "WorkingDirectory": "/tmp", "RunAtLoad": true, "KeepAlive": true
            ]
        ]
        for shape in shapes {
            let data = try PropertyListSerialization.data(fromPropertyList: shape, format: .xml, options: 0)
            try data.write(to: plist)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: plist.path)
            XCTAssertEqual(try writer().ensureInstalled(requireConfig: true), .replaced)
            let parsed = try plistDictionary()
            XCTAssertEqual(parsed["ProgramArguments"] as? [String], [binary.path, "--config", config.path, "serve"])
            XCTAssertNil(parsed["WorkingDirectory"])
        }
    }

    func testWideModePlistIsTightened() throws {
        XCTAssertEqual(try writer().ensureInstalled(requireConfig: true), .created)
        XCTAssertEqual(chmod(plist.path, 0o644), 0)
        XCTAssertEqual(try writer().ensureInstalled(requireConfig: true), .replaced)
        XCTAssertEqual(status(plist).st_mode & 0o777, 0o600)
    }

    func testPathsWithSpacesAmpersandAngleAndQuotesRoundTrip() throws {
        let (_, odd) = try makeBundle(named: "Teslatlas & <Hub> \"Q\" it's.app")
        let folder = home.appendingPathComponent("Library/Application Support/Teslatlas Hub", isDirectory: true)
        let oddConfig = folder.appendingPathComponent("conf & <x> \"y\".toml")
        try Data("data_dir = \"x\"\n".utf8).write(to: oddConfig)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: oddConfig.path)
        _ = try writer(configuration(binary: odd, config: oddConfig)).ensureInstalled(requireConfig: true)
        XCTAssertEqual(try plistDictionary()["ProgramArguments"] as? [String],
                       [odd.path, "--config", oddConfig.path, "serve"])
    }

    // MARK: - Directory and binary policy

    func testLaunchAgentsDirectoryPolicy() throws {
        func directory(mode: mode_t, uid: uid_t = getuid(), gid: gid_t = getgid()) -> stat {
            var entry = stat()
            entry.st_mode = S_IFDIR | mode
            entry.st_uid = uid
            entry.st_gid = gid
            return entry
        }
        let policy = HubDirectoryPolicy.launchAgentsLeaf
        XCTAssertTrue(DevelopmentEventLog.admitsDirectoryEntry(directory(mode: 0o755), leaf: true,
                                                               allowOwnedGroupWritableAncestors: true, policy: policy))
        XCTAssertTrue(DevelopmentEventLog.admitsDirectoryEntry(directory(mode: 0o700), leaf: true,
                                                               allowOwnedGroupWritableAncestors: true, policy: policy))
        XCTAssertFalse(DevelopmentEventLog.admitsDirectoryEntry(directory(mode: 0o775), leaf: true,
                                                                allowOwnedGroupWritableAncestors: true, policy: policy))
        XCTAssertFalse(DevelopmentEventLog.admitsDirectoryEntry(directory(mode: 0o757), leaf: true,
                                                                allowOwnedGroupWritableAncestors: true, policy: policy))
        XCTAssertFalse(DevelopmentEventLog.admitsDirectoryEntry(directory(mode: 0o755, uid: getuid() + 1), leaf: true,
                                                                allowOwnedGroupWritableAncestors: true, policy: policy))
        // The private leaf keeps today's strict rule: 0755 is not 0700.
        XCTAssertFalse(DevelopmentEventLog.admitsDirectoryEntry(directory(mode: 0o755), leaf: true,
                                                                allowOwnedGroupWritableAncestors: true))
        // Bundle ancestors: root:admin 0775 above the bundle passes, writable inside fails.
        let admin = directory(mode: 0o775, uid: 0, gid: 80)
        XCTAssertTrue(HubLaunchAgentPathPolicy.admitsBundleAncestor(admin, ownerUID: getuid(), insideBundle: false))
        XCTAssertFalse(HubLaunchAgentPathPolicy.admitsBundleAncestor(admin, ownerUID: getuid(), insideBundle: true))
        XCTAssertFalse(HubLaunchAgentPathPolicy.admitsBundleAncestor(directory(mode: 0o757), ownerUID: getuid(),
                                                                     insideBundle: false))
        XCTAssertTrue(HubLaunchAgentPathPolicy.admitsBundleAncestor(directory(mode: 0o755), ownerUID: getuid(),
                                                                    insideBundle: true))
        XCTAssertFalse(HubLaunchAgentPathPolicy.admitsBundleAncestor(directory(mode: 0o755, uid: getuid() + 1),
                                                                     ownerUID: getuid(), insideBundle: true))
        // The real file system agrees: a group-writable LaunchAgents leaf is refused.
        try makeDirectory(launchAgents, mode: 0o775)
        XCTAssertThrowsError(try writer().ensureInstalled(requireConfig: true))
        XCTAssertFalse(FileManager.default.fileExists(atPath: plist.path))
    }

    func testSymlinkedLaunchAgentsLeafIsRefused() throws {
        let target = root.appendingPathComponent("elsewhere", isDirectory: true)
        try makeDirectory(target, mode: 0o700)
        try FileManager.default.createSymbolicLink(at: launchAgents, withDestinationURL: target)
        XCTAssertThrowsError(try writer().ensureInstalled(requireConfig: true))
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: target.path), [])
    }

    func testBinaryMissingSymlinkedNonExecutableOrGroupWritableIsRefused() throws {
        let original = try Data(contentsOf: binary)
        // Missing.
        try FileManager.default.removeItem(at: binary)
        XCTAssertThrowsError(try writer().ensureInstalled(requireConfig: true))
        XCTAssertFalse(FileManager.default.fileExists(atPath: plist.path))
        // Symbolic link.
        let real = root.appendingPathComponent("real-binary")
        try original.write(to: real)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: real.path)
        try FileManager.default.createSymbolicLink(at: binary, withDestinationURL: real)
        XCTAssertThrowsError(try writer().ensureInstalled(requireConfig: true))
        XCTAssertFalse(FileManager.default.fileExists(atPath: plist.path))
        try FileManager.default.removeItem(at: binary)
        // Non-executable.
        try original.write(to: binary)
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: binary.path)
        XCTAssertThrowsError(try writer().ensureInstalled(requireConfig: true))
        XCTAssertFalse(FileManager.default.fileExists(atPath: plist.path))
        // Group-writable.
        try FileManager.default.setAttributes([.posixPermissions: 0o775], ofItemAtPath: binary.path)
        XCTAssertThrowsError(try writer().ensureInstalled(requireConfig: true))
        XCTAssertFalse(FileManager.default.fileExists(atPath: plist.path))
        // Back to a valid binary: the same writer then succeeds.
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: binary.path)
        XCTAssertEqual(try writer().ensureInstalled(requireConfig: true), .created)
    }

    func testTranslocatedOrExternalVolumeLocationIsRefusedWithMoveMessage() throws {
        for path in [
            "/private/var/folders/zz/AppTranslocation/ABCD/d/Teslatlas Hub.app/Contents/Resources/teslatlas-hub",
            "/Volumes/External/Teslatlas Hub.app/Contents/Resources/teslatlas-hub"
        ] {
            let subject = writer(configuration(binary: URL(fileURLWithPath: path)))
            XCTAssertThrowsError(try subject.ensureInstalled(requireConfig: false)) { error in
                XCTAssertTrue(error.localizedDescription.contains("Applications folder"), error.localizedDescription)
                // The location refusal itself, not the generic unsafe-binary one a missing path also gives.
                XCTAssertTrue(error.localizedDescription.contains("cannot run its background service from this location"),
                              error.localizedDescription)
            }
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: launchAgents.path))
    }

    // MARK: - Existing property list

    func testUnsafeExistingPlistIsRefusedAndByteIdentical() throws {
        _ = try writer().ensureInstalled(requireConfig: true)
        let valid = try Data(contentsOf: plist)
        let canary = root.appendingPathComponent("canary.plist")
        try valid.write(to: canary)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: canary.path)
        func assertRefused(_ label: String) throws {
            let before = snapshot(home)
            XCTAssertThrowsError(try writer().ensureInstalled(requireConfig: true), label)
            XCTAssertThrowsError(try writer().validateRemovable(), label)
            XCTAssertThrowsError(try writer().removeInstalled(), label)
            XCTAssertFalse(writer().isInstalled, label)
            let after = snapshot(home)
            XCTAssertEqual(before, after, label)
            XCTAssertEqual(try Data(contentsOf: canary), valid, label)
        }
        // Symbolic link.
        try FileManager.default.removeItem(at: plist)
        try FileManager.default.createSymbolicLink(at: plist, withDestinationURL: canary)
        try assertRefused("symlink")
        // Two links.
        try FileManager.default.removeItem(at: plist)
        try valid.write(to: plist)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: plist.path)
        let second = launchAgents.appendingPathComponent("second-name")
        XCTAssertEqual(link(plist.path, second.path), 0)
        try assertRefused("two links")
        XCTAssertEqual(unlink(second.path), 0)
        // A directory.
        try FileManager.default.removeItem(at: plist)
        try makeDirectory(plist, mode: 0o700)
        try assertRefused("directory")
        try FileManager.default.removeItem(at: plist)
        // Another label: removal refuses, ensure replaces (the legacy shapes are replaced).
        let foreign = try PropertyListSerialization.data(fromPropertyList: ["Label": "com.example.other"],
                                                         format: .xml, options: 0)
        try foreign.write(to: plist)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: plist.path)
        XCTAssertThrowsError(try writer().validateRemovable())
        XCTAssertThrowsError(try writer().removeInstalled())
        XCTAssertEqual(try Data(contentsOf: plist), foreign)
        XCTAssertFalse(writer().isInstalled)
        // A foreign owner: the configured owner is not the file's owner.
        try valid.write(to: plist)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: plist.path)
        let stranger = writer(configuration(ownerUID: getuid() + 1))
        XCTAssertThrowsError(try stranger.validateRemovable())
        XCTAssertThrowsError(try stranger.removeInstalled())
        XCTAssertFalse(stranger.isInstalled)
        XCTAssertEqual(try Data(contentsOf: plist), valid)
    }

    func testDescriptorAnchoringSurvivesComponentSwapForWriteAndUnlink() throws {
        let redirected = root.appendingPathComponent("redirected", isDirectory: true)
        try makeDirectory(redirected, mode: 0o700)
        let held = home.appendingPathComponent("Library/LaunchAgents-held", isDirectory: true)
        let swap: (String) -> Void = { [self] opened in
            guard opened == launchAgents.path else { return }
            try? FileManager.default.moveItem(at: launchAgents, to: held)
            try? FileManager.default.createSymbolicLink(at: launchAgents, withDestinationURL: redirected)
        }
        // Write: the swap after the open cannot redirect the write.
        try makeDirectory(launchAgents, mode: 0o700)
        XCTAssertEqual(try writer(hook: swap).ensureInstalled(requireConfig: true), .created)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: redirected.path), [])
        XCTAssertTrue(FileManager.default.fileExists(atPath: held.appendingPathComponent("com.teslatlas.hub.plist").path))
        // Unlink: restore the layout, plant a decoy of the same name in the redirect target.
        try FileManager.default.removeItem(at: launchAgents)
        try FileManager.default.moveItem(at: held, to: launchAgents)
        let decoy = redirected.appendingPathComponent("com.teslatlas.hub.plist")
        let valid = try Data(contentsOf: plist)
        try valid.write(to: decoy)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: decoy.path)
        try writer(hook: swap).removeInstalled()
        XCTAssertEqual(try Data(contentsOf: decoy), valid)
        XCTAssertFalse(FileManager.default.fileExists(atPath: held.appendingPathComponent("com.teslatlas.hub.plist").path))
    }

    func testRemoveUnlinksOnlyThePlistAndIsIdempotent() throws {
        let subject = writer()
        _ = try subject.ensureInstalled(requireConfig: true)
        XCTAssertTrue(subject.isInstalled)
        let neighbour = launchAgents.appendingPathComponent("com.example.neighbour.plist")
        try Data("neighbour".utf8).write(to: neighbour)
        try subject.prepareLogsForBootstrap()
        try Data("log text".utf8).write(to: logs.appendingPathComponent("hub.out.log"))
        let data = try Data(contentsOf: config)
        let neighbourBefore = status(neighbour)
        let logBefore = try Data(contentsOf: logs.appendingPathComponent("hub.out.log"))
        try subject.removeInstalled()
        XCTAssertFalse(FileManager.default.fileExists(atPath: plist.path))
        XCTAssertFalse(subject.isInstalled)
        XCTAssertEqual(try Data(contentsOf: config), data)
        XCTAssertEqual(try Data(contentsOf: logs.appendingPathComponent("hub.out.log")), logBefore)
        XCTAssertEqual(status(neighbour).st_ino, neighbourBefore.st_ino)
        XCTAssertEqual(try Data(contentsOf: neighbour), Data("neighbour".utf8))
        try subject.removeInstalled()
        try writer(configuration()).removeInstalled()
        // An absent LaunchAgents folder is also a success.
        try FileManager.default.removeItem(at: neighbour)
        try FileManager.default.removeItem(at: launchAgents)
        try subject.removeInstalled()
        try subject.validateRemovable()
    }

    func testValidateRemovableChangesNothing() throws {
        let subject = writer()
        _ = try subject.ensureInstalled(requireConfig: true)
        try subject.prepareLogsForBootstrap()
        let before = snapshot(home)
        try subject.validateRemovable()
        XCTAssertEqual(snapshot(home), before)
        XCTAssertTrue(subject.isInstalled)
    }

    // MARK: - Logs

    func testLogsCreatedPrivateAndWideModeRepaired() throws {
        let subject = writer()
        try subject.prepareLogsForBootstrap()
        for name in ["hub.out.log", "hub.err.log"] {
            let information = status(logs.appendingPathComponent(name))
            XCTAssertEqual(information.st_mode & S_IFMT, S_IFREG)
            XCTAssertEqual(information.st_mode & 0o777, 0o600)
            XCTAssertEqual(information.st_nlink, 1)
        }
        XCTAssertEqual(status(logs).st_mode & 0o777, 0o700)
        XCTAssertEqual(chmod(logs.appendingPathComponent("hub.err.log").path, 0o644), 0)
        try subject.prepareLogsForBootstrap()
        XCTAssertEqual(status(logs.appendingPathComponent("hub.err.log")).st_mode & 0o777, 0o600)
        // An existing owner directory that is not writable by others is tightened.
        XCTAssertEqual(chmod(logs.path, 0o755), 0)
        try subject.prepareLogsForBootstrap()
        XCTAssertEqual(status(logs).st_mode & 0o777, 0o700)
    }

    func testUnsafeLogsAreRefusedUntouched() throws {
        try makeDirectory(logs, mode: 0o700)
        let canary = root.appendingPathComponent("log-canary")
        try Data("canary".utf8).write(to: canary)
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: canary.path)
        let out = logs.appendingPathComponent("hub.out.log")
        let err = logs.appendingPathComponent("hub.err.log")
        // Symbolic link.
        try FileManager.default.createSymbolicLink(at: out, withDestinationURL: canary)
        XCTAssertThrowsError(try writer().prepareLogsForBootstrap())
        XCTAssertEqual(status(canary).st_mode & 0o777, 0o644)
        XCTAssertEqual(try Data(contentsOf: canary), Data("canary".utf8))
        try FileManager.default.removeItem(at: out)
        // Two links.
        try Data("x".utf8).write(to: out)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: out.path)
        XCTAssertEqual(link(out.path, home.appendingPathComponent("log-second").path), 0)
        XCTAssertThrowsError(try writer().prepareLogsForBootstrap())
        XCTAssertEqual(status(out).st_mode & 0o777, 0o600)
        XCTAssertEqual(unlink(home.appendingPathComponent("log-second").path), 0)
        // 0640.
        try Data("y".utf8).write(to: err)
        try FileManager.default.setAttributes([.posixPermissions: 0o640], ofItemAtPath: err.path)
        XCTAssertThrowsError(try writer().prepareLogsForBootstrap())
        XCTAssertEqual(status(err).st_mode & 0o777, 0o640)
        XCTAssertEqual(try Data(contentsOf: err), Data("y".utf8))
    }

    func testCompactionKeepsInodeAndExactlyLastHalfMebibyte() throws {
        try makeDirectory(logs, mode: 0o700)
        let out = logs.appendingPathComponent("hub.out.log")
        let total = (1 << 20) + 4_097
        var content = Data(count: total)
        for index in 0..<total { content[index] = UInt8(truncatingIfNeeded: index &* 31 &+ index >> 8) }
        try content.write(to: out)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: out.path)
        let before = status(out)
        try writer().prepareLogsForBootstrap()
        let after = status(out)
        XCTAssertEqual(before.st_ino, after.st_ino)
        XCTAssertEqual(Int(after.st_size), 524_288)
        XCTAssertEqual(try Data(contentsOf: out), content.suffix(524_288))
    }

    func testNoCompactionAtOrBelowOneMebibyte() throws {
        try makeDirectory(logs, mode: 0o700)
        let out = logs.appendingPathComponent("hub.out.log")
        let content = Data(repeating: 0x41, count: 1_048_576)
        try content.write(to: out)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: out.path)
        try writer().prepareLogsForBootstrap()
        XCTAssertEqual(try Data(contentsOf: out), content)
        let smaller = logs.appendingPathComponent("hub.err.log")
        try Data(repeating: 0x42, count: 1_000).write(to: smaller)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: smaller.path)
        try writer().prepareLogsForBootstrap()
        XCTAssertEqual(try Data(contentsOf: smaller), Data(repeating: 0x42, count: 1_000))
    }

    // MARK: - Test-host guard (D-E16)

    func testRealHomeGuardThrowsBeforeAnyFileSystemCall() throws {
        // A valid, installed fixture whose own home stands in for the real home.
        _ = try writer().ensureInstalled(requireConfig: true)
        try writer().prepareLogsForBootstrap()
        let before = snapshot(home)
        var hookCalls = 0
        let guarded = writer(realHome: home, environment: xcTestEnvironment, hook: { _ in hookCalls += 1 })
        XCTAssertThrowsError(try guarded.ensureInstalled(requireConfig: true))
        XCTAssertThrowsError(try guarded.prepareLogsForBootstrap())
        XCTAssertThrowsError(try guarded.validateRemovable())
        XCTAssertThrowsError(try guarded.removeInstalled())
        XCTAssertEqual(hookCalls, 0)
        XCTAssertEqual(snapshot(home), before)
        XCTAssertTrue(FileManager.default.fileExists(atPath: plist.path))
    }

    func testIsInstalledIsFalseForRealHomeUnderXCTest() throws {
        _ = try writer().ensureInstalled(requireConfig: true)
        XCTAssertTrue(writer().isInstalled)
        var hookCalls = 0
        let guarded = writer(realHome: home, environment: xcTestEnvironment, hook: { _ in hookCalls += 1 })
        XCTAssertFalse(guarded.isInstalled)
        XCTAssertEqual(hookCalls, 0)
        // Outside an XCTest host the same identities are not refused.
        XCTAssertTrue(writer(realHome: home, environment: [:]).isInstalled)
    }

    func testRealHomeDefaultIsNSHomeDirectory() {
        XCTAssertEqual(HubTestHost.defaultRealHome().path, HubTestHost.lexicalPath(HubTestScratch.realHomePath))
        let real = HubTestHost.defaultRealHome()
        let aliases = [
            URL(fileURLWithPath: HubTestScratch.realHomePath + "/./"),
            URL(fileURLWithPath: HubTestScratch.realHomePath + "/Library/..")
        ]
        for alias in aliases {
            XCTAssertTrue(HubTestHost.refusesHome(alias, realHome: real, environment: xcTestEnvironment), alias.path)
        }
        XCTAssertFalse(HubTestHost.refusesHome(URL(fileURLWithPath: HubTestScratch.realHomePath + "-other"), realHome: real,
                                               environment: xcTestEnvironment))
        XCTAssertFalse(HubTestHost.refusesHome(real, realHome: real, environment: [:]))
        XCTAssertEqual(HubTestHost.lexicalPath("/a/./b/../c//d/"), "/a/c/d")
    }

    // MARK: - Contract

    /// The value of Rust's `const LABEL: &str = "...";`, nil when the line is absent.
    static func rustLabel(in source: String) -> String? {
        guard let line = source.split(separator: "\n").first(where: { $0.hasPrefix("const LABEL: &str = \"") }) else {
            return nil
        }
        let parts = line.split(separator: "\"", omittingEmptySubsequences: false)
        return parts.count == 3 ? String(parts[1]) : nil
    }

    func testContractLabelMatchesRustLabel() throws {
        XCTAssertEqual(Self.rustLabel(in: "// c\nconst LABEL: &str = \"com.example.x\";\n"), "com.example.x")
        XCTAssertNil(Self.rustLabel(in: "const OTHER: &str = \"x\";"))
        // `LABEL` in src/platform/macos_launch_agent.rs (line 18, re-read 2026-10-14). The
        // focused harness stages only the app folder, so the Rust file is reachable only
        // when the tests run from a checkout; there the label is compared to it as well.
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        if let source = try? String(contentsOf: root.appendingPathComponent("src/platform/macos_launch_agent.rs"),
                                    encoding: .utf8) {
            XCTAssertEqual(HubLaunchAgentContract.label, Self.rustLabel(in: source), "Rust LABEL drifted")
        }
        XCTAssertEqual(HubLaunchAgentContract.label, "com.teslatlas.hub")
        XCTAssertEqual(HubLaunchAgentContract.plistName, "com.teslatlas.hub.plist")
        XCTAssertEqual(HubLaunchAgentContract.identityVariable, "TESLATLAS_HUB_EXECUTABLE_IDENTITY")
        XCTAssertFalse(HubLaunchAgentContract.identityVariable.hasPrefix("TESLATLAS_HUB_DEVELOPMENT"))
    }

    func testContractConstantsDeriveTheServiceWorkBudget() {
        XCTAssertEqual(HubLaunchAgentContract.exitTimeOut, 270)
        XCTAssertEqual(HubLaunchAgentContract.unloadPollDeadline, 300)
        XCTAssertEqual(HubLaunchAgentContract.launchctlRunLimit, 30)
        XCTAssertEqual(HubLaunchAgentContract.replaceStopShare, 390)
        XCTAssertEqual(HubLaunchAgentContract.replaceActionStartShare, 150)
        XCTAssertEqual(HubLaunchAgentContract.replaceRecoveryShare, 120)
        XCTAssertEqual(HubLaunchAgentContract.replaceStartShare, 270)
        XCTAssertEqual(HubLaunchAgentContract.replaceServiceBudget, 660)
        XCTAssertEqual(HubLaunchAgentContract.replaceStopShare,
                       HubLaunchAgentContract.unloadPollDeadline + 3 * HubLaunchAgentContract.launchctlRunLimit)
        XCTAssertEqual(HubLaunchAgentContract.compactAbove, 1_048_576)
        XCTAssertEqual(HubLaunchAgentContract.compactKeep, 524_288)
        XCTAssertEqual(HubLaunchAgentContract.umask, 63)
        XCTAssertEqual(HubLaunchAgentContract.throttleInterval, 30)
    }

    func testIdentityStringUsesVersionsInodeSizeAndNanosecondMtime() {
        var entry = stat()
        entry.st_dev = 77
        entry.st_ino = 12_345
        entry.st_size = 6_789
        entry.st_mtimespec = timespec(tv_sec: 1_790_000_000, tv_nsec: 42)
        XCTAssertEqual(HubLaunchAgentWriter.identity(bundleVersion: "2026.36.2+20260905.1", entry: entry),
                       "2026.36.2+20260905.1:12345:6789:1790000000.000000042")
        entry.st_dev = 78
        XCTAssertEqual(HubLaunchAgentWriter.identity(bundleVersion: "v", entry: entry), "v:12345:6789:1790000000.000000042")
        entry.st_mtimespec.tv_nsec = 999_999_999
        XCTAssertTrue(HubLaunchAgentWriter.identity(bundleVersion: "v", entry: entry).hasSuffix(".999999999"))
    }
}
