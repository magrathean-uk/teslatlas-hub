import Foundation

struct ValidatedContentRange: Equatable, Sendable {
  let start: Int64
  let end: Int64
  let total: Int64?
}

enum RangeResumeValidationError: Error, Equatable, Sendable {
  case invalidRequestedOffset(Int64)
  case unexpectedStatusCode(Int)
  case missingContentRange
  case malformedContentRange
  case invalidBounds
  case mismatchedStart(expected: Int64, actual: Int64)
  case changedEntityTag(expected: String, actual: String?)
}

enum RangeResumeValidator {
  static func validate(
    statusCode: Int,
    contentRange: String?,
    eTag: String?,
    requestedOffset: Int64,
    expectedETag: String?
  ) throws -> ValidatedContentRange {
    guard requestedOffset > 0 else {
      throw RangeResumeValidationError.invalidRequestedOffset(requestedOffset)
    }

    guard statusCode == 206 else {
      throw RangeResumeValidationError.unexpectedStatusCode(statusCode)
    }

    if let expectedETag, eTag != expectedETag {
      throw RangeResumeValidationError.changedEntityTag(
        expected: expectedETag,
        actual: eTag
      )
    }

    guard let contentRange else {
      throw RangeResumeValidationError.missingContentRange
    }

    let parsed = try parse(contentRange)
    guard parsed.start == requestedOffset else {
      throw RangeResumeValidationError.mismatchedStart(
        expected: requestedOffset,
        actual: parsed.start
      )
    }

    return parsed
  }

  private static func parse(_ rawValue: String) throws -> ValidatedContentRange {
    let trimmed = rawValue.trimmingCharacters(in: .whitespaces)
    let unitAndValue = trimmed.split(
      maxSplits: 1,
      omittingEmptySubsequences: true,
      whereSeparator: { $0.isWhitespace }
    )

    guard unitAndValue.count == 2,
      unitAndValue[0].lowercased() == "bytes"
    else {
      throw RangeResumeValidationError.malformedContentRange
    }

    let rangeAndTotal = unitAndValue[1].split(
      separator: "/",
      maxSplits: 1,
      omittingEmptySubsequences: false
    )
    guard rangeAndTotal.count == 2 else {
      throw RangeResumeValidationError.malformedContentRange
    }

    let bounds = rangeAndTotal[0].split(
      separator: "-",
      maxSplits: 1,
      omittingEmptySubsequences: false
    )
    guard bounds.count == 2,
      let start = parseUnsignedDecimal(bounds[0]),
      let end = parseUnsignedDecimal(bounds[1])
    else {
      throw RangeResumeValidationError.malformedContentRange
    }

    let totalText = rangeAndTotal[1]
    let total: Int64?
    if totalText == "*" {
      total = nil
    } else if let parsedTotal = parseUnsignedDecimal(totalText) {
      total = parsedTotal
    } else {
      throw RangeResumeValidationError.malformedContentRange
    }

    guard start >= 0, end >= start else {
      throw RangeResumeValidationError.invalidBounds
    }

    if let total {
      guard total > 0, end < total else {
        throw RangeResumeValidationError.invalidBounds
      }
    }

    return ValidatedContentRange(start: start, end: end, total: total)
  }

  private static func parseUnsignedDecimal(_ value: Substring) -> Int64? {
    guard !value.isEmpty,
      value.utf8.allSatisfy({ (0x30...0x39).contains($0) })
    else {
      return nil
    }
    return Int64(value)
  }
}
