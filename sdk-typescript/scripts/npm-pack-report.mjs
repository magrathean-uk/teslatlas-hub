export function normalizeNpmPackReport(value, packageName) {
  let report;
  if (Array.isArray(value)) {
    if (value.length !== 1) throw new Error("npm pack did not return exactly one report");
    [report] = value;
  } else if (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    Object.keys(value).length === 1 &&
    Object.hasOwn(value, packageName)
  ) {
    report = value[packageName];
  } else {
    throw new Error("npm pack report shape is invalid");
  }

  if (
    report === null ||
    typeof report !== "object" ||
    Array.isArray(report) ||
    report.name !== packageName
  ) {
    throw new Error("npm pack report package identity is invalid");
  }
  return report;
}
