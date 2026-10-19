// SPDX-License-Identifier: AGPL-3.0-only

import Darwin
import Foundation

/// R-E1F6-22: the only place a test fixture root may come from.
///
/// A test run must never create anything under the owner's home (the machine rule is that
/// nothing new appears in `~`). Three kinds of root, none of them below the home:
///
/// * `fixtureRoot(_:)`: `teslatlas-hub-tests/<Name>-<UUID>` in the per-user Darwin temporary
///   directory (`confstr(_CS_DARWIN_USER_TEMP_DIR)`, below `/private/var/folders`; the parent
///   is created 0700). For fixtures the app never walks with its own private-path rules.
/// * `validatedFixtureRoot(_:)`: `<volume>/<Name>-<UUID>` on the small disk image the test script
///   mounts for the run (`TESLATLAS_HUB_TEST_VOLUME`). The app's own rules (a descriptor walk
///   from `/` that refuses links, group-writable ancestors and a path that is not its
///   standardised form) refuse every path below the Darwin temporary directory (`/var` is a
///   link, `/private/var` is not the standardised spelling) and every path on the shared
///   external volume, so fixtures that go through those rules live on the image.
/// * An app bundle that the LaunchAgent writer walks (no link on the way, not on `/Volumes/`) also
///   lives in the Darwin folder: it is resolved through `realpath`, so no component is a link.
///
/// The test scripts remove exactly the Darwin folder and unmount the image after every run.
enum HubTestScratch {
    static let folderName = "teslatlas-hub-tests"

    /// The owner's real home, for comparing or refusing only (what the code under test must never touch).
    /// A test never creates, writes or removes anything below it; the source scan enforces that.
    static var realHomePath: String { NSHomeDirectory() }

    /// The per-user Darwin temporary directory with `/var` resolved to `/private/var`.
    static let darwinTemporaryDirectory: URL = {
        var buffer = [CChar](repeating: 0, count: Int(PATH_MAX) + 1)
        let length = confstr(_CS_DARWIN_USER_TEMP_DIR, &buffer, buffer.count)
        precondition(length > 1 && length <= buffer.count, "confstr(_CS_DARWIN_USER_TEMP_DIR) failed")
        let url = URL(fileURLWithPath: realPath(String(cString: buffer)), isDirectory: true)
        precondition(!isUnderRealHome(url), "the Darwin temporary directory is under the real home: \(url.path)")
        return url
    }()

    /// `teslatlas-hub-tests` inside the Darwin temporary directory, created 0700 on first use.
    static let directory: URL = {
        let url = darwinTemporaryDirectory.appendingPathComponent(folderName, isDirectory: true)
        precondition(!isUnderRealHome(url), "test scratch resolved under the real home: \(url.path)")
        var info = stat()
        if lstat(url.path, &info) != 0 {
            precondition(mkdir(url.path, 0o700) == 0 || errno == EEXIST, "cannot create \(url.path): errno \(errno)")
            precondition(lstat(url.path, &info) == 0, "cannot inspect \(url.path)")
        }
        precondition((info.st_mode & S_IFMT) == S_IFDIR, "\(url.path) is not a real folder")
        precondition(info.st_uid == getuid(), "\(url.path) belongs to another user")
        precondition(chmod(url.path, 0o700) == 0, "cannot restrict \(url.path)")
        return url
    }()

    /// A fresh fixture root `directory/<name>-<UUID>`. The folder is not created; the caller does that.
    static func fixtureRoot(_ name: String) -> URL {
        root(in: directory, name)
    }

    /// The disk image the test script mounted for this run.
    static var validatedVolume: URL {
        guard let path = ProcessInfo.processInfo.environment[volumeVariable], path.hasPrefix("/Volumes/TeslatlasHubTests-") else {
            preconditionFailure("\(volumeVariable) is not set: run the AppKit tests through scripts/test-macos-appkit(-focused).sh, "
                + "which mounts the private test volume the app's path rules need")
        }
        let url = URL(fileURLWithPath: path, isDirectory: true)
        var info = stat()
        precondition(lstat(url.path, &info) == 0 && (info.st_mode & S_IFMT) == S_IFDIR, "\(path) is not mounted")
        precondition(!isUnderRealHome(url), "the test volume is under the real home: \(path)")
        return url
    }

    /// A fresh fixture root on the mounted test volume, for fixtures the app validates with its own path rules.
    static func validatedFixtureRoot(_ name: String) -> URL {
        root(in: validatedVolume, name)
    }

    static let volumeVariable = "TESLATLAS_HUB_TEST_VOLUME"

    private static func root(in parent: URL, _ name: String) -> URL {
        precondition(!name.isEmpty && !name.contains("/"), "a fixture name is one path component")
        let url = parent.appendingPathComponent("\(name)-\(UUID().uuidString)", isDirectory: true)
        precondition(!isUnderRealHome(url), "a fixture root resolved under the real home: \(url.path)")
        return url
    }

    /// True when `url` is the real home or inside it, by its lexical path and by its resolved path.
    static func isUnderRealHome(_ url: URL) -> Bool {
        let homes = Set([NSHomeDirectory(), realPath(NSHomeDirectory())].map { standardised($0) })
        let candidates = [standardised(url.path), standardised(realPath(url.path))]
        return candidates.contains { path in
            homes.contains { path == $0 || path.hasPrefix($0 + "/") }
        }
    }

    /// realpath(3) (Foundation's resolvingSymlinksInPath folds /private/var back to /var); the input when it cannot resolve.
    private static func realPath(_ path: String) -> String {
        guard let resolved = realpath(path, nil) else { return path }
        defer { free(resolved) }
        return String(cString: resolved)
    }

    private static func standardised(_ path: String) -> String {
        var parts: [Substring] = []
        for part in path.split(separator: "/", omittingEmptySubsequences: true) {
            if part == "." { continue }
            if part == ".." { _ = parts.popLast(); continue }
            parts.append(part)
        }
        return "/" + parts.joined(separator: "/")
    }
}
