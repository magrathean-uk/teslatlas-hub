import Foundation
import AppKit
import Virtualization

enum LauncherError: Error, CustomStringConvertible {
    case usage(String)
    case invalid(String)

    var description: String {
        switch self {
        case .usage(let message), .invalid(let message): return message
        }
    }
}

struct Options {
    let iso: URL
    let disk: URL
    let efiVariables: URL
    let createDisk: Bool
    let createEFI: Bool
    let start: Bool
}

let gibibyte: UInt64 = 1_073_741_824
let requiredDiskBytes = 50 * gibibyte
let usage = """
Usage:
  launch.sh --iso PATH --disk PATH --efi-vars PATH [--create-disk] [--create-efi] [--validate]
  launch.sh --iso PATH --disk PATH --efi-vars PATH --start

Debian arm64 Apple Virtualization bench:
  CPU: 8 vCPU    RAM: 8 GiB    disk: at least 50 GiB

The default is validation only. --start is required to run the VM.
The ISO, disk, and EFI variable-store paths must be explicit.
--create-disk creates one new sparse 50 GiB raw disk. --create-efi creates one
new EFI variable store. Neither flag overwrites an existing path.
"""

func parseOptions() throws -> Options {
    var iso: URL?
    var disk: URL?
    var efiVariables: URL?
    var createDisk = false
    var createEFI = false
    var start = false
    var index = 1

    while index < CommandLine.arguments.count {
        let argument = CommandLine.arguments[index]
        switch argument {
        case "--help", "-h":
            print(usage)
            exit(0)
        case "--start":
            start = true
        case "--create-disk":
            createDisk = true
        case "--create-efi":
            createEFI = true
        case "--validate":
            break
        case "--iso", "--disk", "--efi-vars":
            guard index + 1 < CommandLine.arguments.count else {
                throw LauncherError.usage("Missing value for \(argument)\n\n\(usage)")
            }
            let value = URL(fileURLWithPath: CommandLine.arguments[index + 1]).standardizedFileURL
            switch argument {
            case "--iso": iso = value
            case "--disk": disk = value
            default: efiVariables = value
            }
            index += 1
        default:
            throw LauncherError.usage("Unknown argument: \(argument)\n\n\(usage)")
        }
        index += 1
    }

    guard let iso, let disk, let efiVariables else {
        throw LauncherError.usage("--iso, --disk, and --efi-vars are required\n\n\(usage)")
    }
    return Options(
        iso: iso,
        disk: disk,
        efiVariables: efiVariables,
        createDisk: createDisk,
        createEFI: createEFI,
        start: start
    )
}

func requireRegularFile(_ url: URL, label: String) throws -> UInt64 {
    var isDirectory: ObjCBool = false
    guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory), !isDirectory.boolValue else {
        throw LauncherError.invalid("\(label) is missing or is a directory: \(url.path)")
    }
    let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
    guard let size = attributes[.size] as? NSNumber else {
        throw LauncherError.invalid("Cannot read \(label) size: \(url.path)")
    }
    return size.uint64Value
}

func createSparseFile(_ url: URL, label: String, bytes: UInt64) throws {
    guard !FileManager.default.fileExists(atPath: url.path) else {
        throw LauncherError.invalid("Refusing to overwrite existing \(label): \(url.path)")
    }
    try FileManager.default.createDirectory(
        at: url.deletingLastPathComponent(),
        withIntermediateDirectories: true,
        attributes: nil
    )
    guard FileManager.default.createFile(atPath: url.path, contents: nil) else {
        throw LauncherError.invalid("Cannot create \(label): \(url.path)")
    }
    do {
        let handle = try FileHandle(forWritingTo: url)
        try handle.truncate(atOffset: bytes)
        try handle.close()
    } catch {
        try? FileManager.default.removeItem(at: url)
        throw error
    }
}

func prepareDisk(_ options: Options) throws -> UInt64 {
    if !FileManager.default.fileExists(atPath: options.disk.path) {
        guard options.createDisk else {
            throw LauncherError.invalid("Disk is missing: \(options.disk.path). Use --create-disk to create a new sparse 50 GiB disk.")
        }
        try createSparseFile(options.disk, label: "disk", bytes: requiredDiskBytes)
    }
    return try requireRegularFile(options.disk, label: "disk")
}

func loadOrCreateEFIStore(_ options: Options) throws -> VZEFIVariableStore {
    if FileManager.default.fileExists(atPath: options.efiVariables.path) {
        return VZEFIVariableStore(url: options.efiVariables)
    }
    guard options.createEFI else {
        throw LauncherError.invalid("EFI variable store is missing: \(options.efiVariables.path). Use --create-efi to create one.")
    }
    try FileManager.default.createDirectory(
        at: options.efiVariables.deletingLastPathComponent(),
        withIntermediateDirectories: true,
        attributes: nil
    )
    return try VZEFIVariableStore(
        creatingVariableStoreAt: options.efiVariables,
        options: []
    )
}

func makeConfiguration(_ options: Options) throws -> VZVirtualMachineConfiguration {
    let isoSize = try requireRegularFile(options.iso, label: "ISO")
    let diskSize = try prepareDisk(options)
    guard isoSize > 0 else { throw LauncherError.invalid("ISO is empty: \(options.iso.path)") }
    guard diskSize >= requiredDiskBytes else {
        throw LauncherError.invalid("Disk must be at least 50 GiB; found \(diskSize / gibibyte) GiB")
    }

    let configuration = VZVirtualMachineConfiguration()
    configuration.cpuCount = 8
    configuration.memorySize = 8 * gibibyte
    configuration.platform = VZGenericPlatformConfiguration()

    let bootLoader = VZEFIBootLoader()
    bootLoader.variableStore = try loadOrCreateEFIStore(options)
    configuration.bootLoader = bootLoader

    let diskAttachment = try VZDiskImageStorageDeviceAttachment(url: options.disk, readOnly: false)
    configuration.storageDevices = [VZVirtioBlockDeviceConfiguration(attachment: diskAttachment)]

    let isoAttachment = try VZDiskImageStorageDeviceAttachment(url: options.iso, readOnly: true)
    configuration.storageDevices.append(VZUSBMassStorageDeviceConfiguration(attachment: isoAttachment))
    configuration.usbControllers = [VZXHCIControllerConfiguration()]

    let graphics = VZVirtioGraphicsDeviceConfiguration()
    graphics.scanouts = [
        VZVirtioGraphicsScanoutConfiguration(widthInPixels: 1440, heightInPixels: 900),
    ]
    configuration.graphicsDevices = [graphics]
    configuration.keyboards = [VZUSBKeyboardConfiguration()]
    configuration.pointingDevices = [VZUSBScreenCoordinatePointingDeviceConfiguration()]

    let network = VZVirtioNetworkDeviceConfiguration()
    network.attachment = VZNATNetworkDeviceAttachment()
    configuration.networkDevices = [network]

    let console = VZVirtioConsoleDeviceSerialPortConfiguration()
    console.attachment = VZFileHandleSerialPortAttachment(
        fileHandleForReading: FileHandle.standardInput,
        fileHandleForWriting: FileHandle.standardOutput
    )
    configuration.serialPorts = [console]
    try configuration.validate()
    return configuration
}

final class VMWindowController: NSObject, NSApplicationDelegate {
    private let configuration: VZVirtualMachineConfiguration
    private var virtualMachine: VZVirtualMachine?
    private var window: NSWindow?

    init(configuration: VZVirtualMachineConfiguration) {
        self.configuration = configuration
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let virtualMachine = VZVirtualMachine(configuration: configuration)
        let view = VZVirtualMachineView(frame: NSRect(x: 0, y: 0, width: 1440, height: 900))
        view.virtualMachine = virtualMachine
        view.automaticallyReconfiguresDisplay = true
        view.autoresizingMask = [.width, .height]

        let window = NSWindow(
            contentRect: view.frame,
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Teslatlas Hub Debian arm64 bench"
        window.contentView = view
        window.makeKeyAndOrderFront(nil)

        self.virtualMachine = virtualMachine
        self.window = window
        virtualMachine.start { result in
            if case .failure(let error) = result {
                fputs("VM start failed: \(error)\n", stderr)
                NSApp.terminate(nil)
            }
        }
    }
}

do {
    let options = try parseOptions()
    let configuration = try makeConfiguration(options)
    print("Validated Apple Virtualization Debian arm64 bench: 8 vCPU, 8 GiB RAM, disk >= 50 GiB")
    guard options.start else { exit(0) }
    guard VZVirtualMachine.isSupported else {
        throw LauncherError.invalid("Apple Virtualization is unavailable on this Mac")
    }
    let application = NSApplication.shared
    application.setActivationPolicy(.regular)
    let controller = VMWindowController(configuration: configuration)
    application.delegate = controller
    application.activate(ignoringOtherApps: true)
    withExtendedLifetime(controller) {
        application.run()
    }
} catch {
    fputs("Error: \(error)\n", stderr)
    exit(2)
}
