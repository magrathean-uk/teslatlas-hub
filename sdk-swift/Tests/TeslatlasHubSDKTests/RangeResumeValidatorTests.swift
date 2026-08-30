import XCTest

@testable import TeslatlasHubSDK

final class RangeResumeValidatorTests: XCTestCase {
  func testAcceptsMatchingPartialResponse() throws {
    let result = try RangeResumeValidator.validate(
      statusCode: 206,
      contentRange: "bytes 10-19/100",
      eTag: "\"pack-revision-7\"",
      requestedOffset: 10,
      expectedETag: "\"pack-revision-7\""
    )

    XCTAssertEqual(
      result,
      ValidatedContentRange(start: 10, end: 19, total: 100)
    )
  }

  func testAcceptsUnknownCompleteLengthWithoutInventingManifestRules() throws {
    let result = try RangeResumeValidator.validate(
      statusCode: 206,
      contentRange: "bytes 10-19/*",
      eTag: nil,
      requestedOffset: 10,
      expectedETag: nil
    )

    XCTAssertEqual(
      result,
      ValidatedContentRange(start: 10, end: 19, total: nil)
    )
  }

  func testRejectsNonPartialStatusForResume() {
    XCTAssertThrowsError(
      try RangeResumeValidator.validate(
        statusCode: 200,
        contentRange: nil,
        eTag: "\"same\"",
        requestedOffset: 10,
        expectedETag: "\"same\""
      )
    ) { error in
      XCTAssertEqual(
        error as? RangeResumeValidationError,
        .unexpectedStatusCode(200)
      )
    }
  }

  func testRejectsResponseStartingAtWrongOffset() {
    XCTAssertThrowsError(
      try RangeResumeValidator.validate(
        statusCode: 206,
        contentRange: "bytes 0-19/100",
        eTag: nil,
        requestedOffset: 10,
        expectedETag: nil
      )
    ) { error in
      XCTAssertEqual(
        error as? RangeResumeValidationError,
        .mismatchedStart(expected: 10, actual: 0)
      )
    }
  }

  func testRejectsMissingOrMalformedContentRange() {
    let malformedValues: [String?] = [
      nil,
      "items 10-19/100",
      "bytes ten-19/100",
      "bytes 10/100",
      "bytes 10-19",
      "bytes +10-19/100",
      "bytes 10-+19/100",
      "bytes 10-19/+100",
    ]

    for contentRange in malformedValues {
      XCTAssertThrowsError(
        try RangeResumeValidator.validate(
          statusCode: 206,
          contentRange: contentRange,
          eTag: nil,
          requestedOffset: 10,
          expectedETag: nil
        ),
        "Expected rejection for \(String(describing: contentRange))"
      )
    }
  }

  func testRejectsImpossibleRangeBounds() {
    let impossibleValues = [
      "bytes 10-9/100",
      "bytes 10-19/19",
      "bytes 10-19/0",
      "bytes -1-19/100",
    ]

    for contentRange in impossibleValues {
      XCTAssertThrowsError(
        try RangeResumeValidator.validate(
          statusCode: 206,
          contentRange: contentRange,
          eTag: nil,
          requestedOffset: 10,
          expectedETag: nil
        ),
        "Expected rejection for \(contentRange)"
      )
    }
  }

  func testRejectsChangedOrMissingEntityTag() {
    for actualETag in ["\"changed\"", nil] as [String?] {
      XCTAssertThrowsError(
        try RangeResumeValidator.validate(
          statusCode: 206,
          contentRange: "bytes 10-19/100",
          eTag: actualETag,
          requestedOffset: 10,
          expectedETag: "\"original\""
        )
      ) { error in
        XCTAssertEqual(
          error as? RangeResumeValidationError,
          .changedEntityTag(expected: "\"original\"", actual: actualETag)
        )
      }
    }
  }
}
