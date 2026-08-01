import Foundation
import Security

func fail(_ message: String, status: OSStatus? = nil) -> Never {
    if let status {
        let detail = SecCopyErrorMessageString(status, nil) as String? ?? "unknown"
        FileHandle.standardError.write(Data("\(message): \(detail)\n".utf8))
    } else {
        FileHandle.standardError.write(Data("\(message)\n".utf8))
    }
    exit(1)
}

guard CommandLine.arguments.count == 4 else {
    fail("usage: teslatlas-hub-keychain get|set|exists SERVICE ACCOUNT")
}

let command = CommandLine.arguments[1]
let service = CommandLine.arguments[2]
let account = CommandLine.arguments[3]
let base: [String: Any] = [
    kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: service,
    kSecAttrAccount as String: account,
]

switch command {
case "set":
    let secret = FileHandle.standardInput.readDataToEndOfFile()
    guard !secret.isEmpty else {
        fail("refusing empty keychain value")
    }
    let attributes: [String: Any] = [
        kSecValueData as String: secret,
        kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
    ]
    let updateStatus = SecItemUpdate(base as CFDictionary, attributes as CFDictionary)
    if updateStatus == errSecSuccess {
        break
    }
    guard updateStatus == errSecItemNotFound else {
        fail("cannot update keychain value", status: updateStatus)
    }
    var item = base
    item.merge(attributes) { _, new in new }
    let addStatus = SecItemAdd(item as CFDictionary, nil)
    guard addStatus == errSecSuccess else {
        fail("cannot store keychain value", status: addStatus)
    }
case "get":
    var query = base
    query[kSecReturnData as String] = true
    query[kSecMatchLimit as String] = kSecMatchLimitOne
    var result: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &result)
    guard status == errSecSuccess, let secret = result as? Data else {
        fail("cannot read keychain value", status: status)
    }
    FileHandle.standardOutput.write(secret)
case "exists":
    let status = SecItemCopyMatching(base as CFDictionary, nil)
    if status == errSecSuccess {
        exit(0)
    }
    if status == errSecItemNotFound {
        exit(1)
    }
    fail("cannot inspect keychain value", status: status)
default:
    fail("unknown command")
}
