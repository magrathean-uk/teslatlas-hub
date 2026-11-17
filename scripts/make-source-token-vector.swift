// SPDX-License-Identifier: AGPL-3.0-only
//
// M09-T16 (review B1): writes tests/fixtures/source-token-envelope-vector.json, a fixed
// source-token envelope pair made by Apple CryptoKit, an AES-GCM implementation other than
// the Rust crate the Hub decrypts with. Every input is synthetic and fixed: the key and both
// tokens say so in their text, and the nonces are counting bytes, so a rerun prints the same
// file. The envelope is the documented Cloak V1 layout the Hub reads: type byte 0x01, tag
// length 0x0a, tag "AES.GCM.V1", 12-byte IV, 16-byte GCM tag, then the ciphertext; the cipher
// key is SHA-256 of the exact key bytes and the associated data is "AES256GCM".
//
// Run from the repository root: swift scripts/make-source-token-vector.swift > tests/fixtures/source-token-envelope-vector.json

import CryptoKit
import Foundation

let encryptionKey = "synthetic-encryption-key-for-a-test-vector-only"
let accessToken = "synthetic-access-token-not-a-tesla-login-0001"
let refreshToken = "synthetic-refresh-token-not-a-tesla-login-0001"

func hex(_ bytes: some Sequence<UInt8>) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}

func envelope(_ plaintext: String, nonceStart: UInt8) throws -> String {
    let key = SymmetricKey(data: SHA256.hash(data: Data(encryptionKey.utf8)))
    let nonceBytes = (0..<12).map { nonceStart &+ UInt8($0) }
    let sealed = try AES.GCM.seal(Data(plaintext.utf8), using: key,
                                  nonce: try AES.GCM.Nonce(data: nonceBytes),
                                  authenticating: Data("AES256GCM".utf8))
    var bytes: [UInt8] = [0x01, 0x0a]
    bytes += Array("AES.GCM.V1".utf8)
    bytes += nonceBytes
    bytes += Array(sealed.tag)
    bytes += Array(sealed.ciphertext)
    return hex(bytes)
}

let lines = [
    "{",
    "  \"producer\": \"Apple CryptoKit AES.GCM via scripts/make-source-token-vector.swift\",",
    "  \"encryptionKey\": \"\(encryptionKey)\",",
    "  \"accessToken\": \"\(accessToken)\",",
    "  \"refreshToken\": \"\(refreshToken)\",",
    "  \"accessEnvelopeHex\": \"\(try envelope(accessToken, nonceStart: 0x10))\",",
    "  \"refreshEnvelopeHex\": \"\(try envelope(refreshToken, nonceStart: 0x30))\"",
    "}",
]
print(lines.joined(separator: "\n"))
