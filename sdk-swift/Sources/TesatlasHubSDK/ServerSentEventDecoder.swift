import Foundation

struct ServerSentEvent: Equatable, Sendable {
  let id: String?
  let name: String?
  let data: String
}

enum ServerSentEventDecoderOutput: Equatable, Sendable {
  case event(ServerSentEvent)
  case retry(milliseconds: UInt64)
  case replayReset
}

struct ServerSentEventDecoderLimits: Equatable, Sendable {
  let maximumLineBytes: Int
  let maximumEventDataBytes: Int

  init(
    maximumLineBytes: Int = 8 * 1_024 * 1_024 + 6,
    maximumEventDataBytes: Int = 8 * 1_024 * 1_024
  ) {
    precondition(maximumLineBytes > 0)
    precondition(maximumEventDataBytes > 0)
    self.maximumLineBytes = maximumLineBytes
    self.maximumEventDataBytes = maximumEventDataBytes
  }
}

enum ServerSentEventDecodingError: Error, Equatable {
  case invalidUTF8
  case lineTooLong(limit: Int)
  case eventDataTooLarge(limit: Int)
}

struct ServerSentEventDecoder: Sendable {
  private let limits: ServerSentEventDecoderLimits
  private var bufferedBytes: [UInt8] = []
  private var pendingCarriageReturn = false
  private var dataLines: [String] = []
  private var eventDataByteCount = 0
  private var eventName: String?
  private var lastEventID: String?

  init(limits: ServerSentEventDecoderLimits = ServerSentEventDecoderLimits()) {
    self.limits = limits
  }

  mutating func append(_ data: Data) throws -> [ServerSentEventDecoderOutput] {
    do {
      var output: [ServerSentEventDecoderOutput] = []
      for byte in data {
        if pendingCarriageReturn {
          pendingCarriageReturn = false
          output.append(contentsOf: try completeLine())
          if byte == 0x0A { continue }
        }

        switch byte {
        case 0x0A:
          output.append(contentsOf: try completeLine())
        case 0x0D:
          pendingCarriageReturn = true
        default:
          guard bufferedBytes.count < limits.maximumLineBytes else {
            throw ServerSentEventDecodingError.lineTooLong(
              limit: limits.maximumLineBytes
            )
          }
          bufferedBytes.append(byte)
        }
      }
      return output
    } catch {
      reset()
      throw error
    }
  }

  mutating func finish() throws -> [ServerSentEventDecoderOutput] {
    do {
      var output: [ServerSentEventDecoderOutput] = []
      if pendingCarriageReturn {
        pendingCarriageReturn = false
        output.append(contentsOf: try completeLine())
      }

      if !bufferedBytes.isEmpty {
        output.append(contentsOf: try processLineBytes(bufferedBytes))
      }

      reset()
      return output
    } catch {
      reset()
      throw error
    }
  }

  private mutating func completeLine() throws -> [ServerSentEventDecoderOutput] {
    let output = try processLineBytes(bufferedBytes)
    bufferedBytes.removeAll(keepingCapacity: true)
    return output
  }

  private mutating func processLineBytes(
    _ bytes: [UInt8]
  ) throws -> [ServerSentEventDecoderOutput] {
    guard bytes.count <= limits.maximumLineBytes else {
      throw ServerSentEventDecodingError.lineTooLong(
        limit: limits.maximumLineBytes
      )
    }

    guard let line = String(bytes: bytes, encoding: .utf8) else {
      throw ServerSentEventDecodingError.invalidUTF8
    }

    guard !line.isEmpty else {
      defer {
        dataLines.removeAll(keepingCapacity: true)
        eventDataByteCount = 0
        eventName = nil
      }

      guard !dataLines.isEmpty else {
        return []
      }

      return [
        .event(
          ServerSentEvent(
            id: lastEventID,
            name: eventName,
            data: dataLines.joined(separator: "\n")
          )
        )
      ]
    }

    guard line.first != ":" else {
      return []
    }

    let field: Substring
    var value: Substring
    if let colonIndex = line.firstIndex(of: ":") {
      field = line[..<colonIndex]
      value = line[line.index(after: colonIndex)...]
      if value.first == " " {
        value = value.dropFirst()
      }
    } else {
      field = Substring(line)
      value = ""
    }

    switch field {
    case "data":
      let separatorByteCount = dataLines.isEmpty ? 0 : 1
      let (withSeparator, separatorOverflow) =
        eventDataByteCount
        .addingReportingOverflow(separatorByteCount)
      let (newByteCount, valueOverflow) =
        withSeparator
        .addingReportingOverflow(value.utf8.count)
      guard !separatorOverflow,
        !valueOverflow,
        newByteCount <= limits.maximumEventDataBytes
      else {
        throw ServerSentEventDecodingError.eventDataTooLarge(
          limit: limits.maximumEventDataBytes
        )
      }
      eventDataByteCount = newByteCount
      dataLines.append(String(value))
    case "event":
      eventName = value.isEmpty ? nil : String(value)
    case "id":
      guard !value.contains("\0") else {
        return []
      }
      lastEventID = value.isEmpty ? nil : String(value)
      if value.isEmpty { return [.replayReset] }
    case "retry":
      let bytes = value.utf8
      guard !bytes.isEmpty,
        bytes.allSatisfy({ (0x30...0x39).contains($0) }),
        let milliseconds = UInt64(value)
      else {
        return []
      }
      return [.retry(milliseconds: milliseconds)]
    default:
      break
    }

    return []
  }

  private mutating func reset() {
    bufferedBytes.removeAll(keepingCapacity: false)
    pendingCarriageReturn = false
    dataLines.removeAll(keepingCapacity: false)
    eventDataByteCount = 0
    eventName = nil
    lastEventID = nil
  }
}
