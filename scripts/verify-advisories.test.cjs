const test = require("node:test");
const assert = require("node:assert/strict");
const { advisoriesFromNpm, advisoriesFromRustSec, verify } = require("./verify-advisories.cjs");

const TODAY = "2026-09-23";
const reviewedNpmException = { ecosystem: "npm", advisory: "GHSA-JRC7-96C5-Q579", owner: "security@example.test", reviewed_by: "reviewer@example.test", reviewed_at: "2026-09-22", rationale: "Tracked only while the owner reviews the breaking remediation.", expires: "2026-10-01" };
function npmFinding(name, id = "GHSA-jrc7-96c5-q579") { return { name, severity: "critical", isDirect: true, via: [{ url: `https://github.com/advisories/${id}` }], effects: [], range: "*", nodes: [] }; }
function npmReport(entries = { maplibre: npmFinding("maplibre") }) { return { auditReportVersion: 2, vulnerabilities: entries, metadata: { vulnerabilities: { info: 0, low: 0, moderate: 0, high: 0, critical: Object.keys(entries).length, total: Object.keys(entries).length } } }; }
function rustsecReport(list = [{ advisory: { id: "RUSTSEC-2026-0001" } }]) { return { vulnerabilities: { found: list.length > 0, count: list.length, list } }; }

test("accepts valid direct and transitive npm v2 reports", () => {
  assert.deepEqual([...advisoriesFromNpm(npmReport())], ["GHSA-JRC7-96C5-Q579"]);
  assert.deepEqual([...advisoriesFromNpm(npmReport({ root: { ...npmFinding("root"), via: ["leaf"] }, leaf: npmFinding("leaf") }))], ["GHSA-JRC7-96C5-Q579"]);
});
test("requires exact active reviewed findings", () => {
  assert.deepEqual(verify({ ecosystem: "npm", report: npmReport(), config: { version: 1, exceptions: [reviewedNpmException] }, scannerExit: 1, today: TODAY }), ["GHSA-JRC7-96C5-Q579"]);
  assert.throws(() => verify({ ecosystem: "npm", report: npmReport({ maplibre: npmFinding("maplibre"), other: npmFinding("other", "GHSA-2mjx-qc3c-rqvc") }), config: { version: 1, exceptions: [reviewedNpmException] }, scannerExit: 1, today: TODAY }), /unreviewed/);
});
test("rejects malformed npm inventories and summaries", () => {
  assert.throws(() => advisoriesFromNpm({ auditReportVersion: 2, vulnerabilities: [], metadata: { vulnerabilities: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 } } }), /report/);
  const badTotal = npmReport(); badTotal.metadata.vulnerabilities.total = 2; assert.throws(() => advisoriesFromNpm(badTotal), /severity counts/);
  const badInventory = npmReport(); badInventory.metadata.vulnerabilities.critical = 2; badInventory.metadata.vulnerabilities.total = 2; assert.throws(() => advisoriesFromNpm(badInventory), /inventory/);
  const badSeverity = npmReport({ maplibre: { ...npmFinding("maplibre"), severity: "low" } }); assert.throws(() => advisoriesFromNpm(badSeverity), /severity counts disagree with vulnerability inventory/);
});
test("rejects unresolved npm advisory objects and meta references", () => {
  assert.throws(() => advisoriesFromNpm(npmReport({ bad: { ...npmFinding("bad"), via: [{ url: "https://example.test/CVE-2026-1" }] } })), /exact GHSA/);
  assert.throws(() => advisoriesFromNpm(npmReport({ bad: { ...npmFinding("bad"), via: ["missing"] } })), /references missing/);
  assert.throws(() => advisoriesFromNpm(npmReport({ a: { ...npmFinding("a"), via: ["b"] }, b: { ...npmFinding("b"), via: ["a"] } })), /cycle/);
});
test("validates RustSec count, finding shape, and exact IDs", () => {
  assert.deepEqual([...advisoriesFromRustSec(rustsecReport())], ["RUSTSEC-2026-0001"]);
  assert.throws(() => advisoriesFromRustSec({ vulnerabilities: { found: true, count: 2, list: [{ advisory: { id: "RUSTSEC-2026-0001" } }] } }), /report/);
  assert.throws(() => advisoriesFromRustSec(rustsecReport([{ advisory: { id: "GHSA-jrc7-96c5-q579" } }])), /exact RustSec/);
  assert.throws(() => advisoriesFromRustSec(rustsecReport([null])), /malformed/);
});
test("rejects stale, malformed, future, and unused exceptions", () => {
  assert.throws(() => verify({ ecosystem: "npm", report: npmReport(), config: { version: 1, exceptions: [] }, scannerExit: 1, today: TODAY }), /unreviewed/);
  assert.throws(() => verify({ ecosystem: "npm", report: npmReport(), config: { version: 1, exceptions: [{ ...reviewedNpmException, expires: TODAY }] }, scannerExit: 1, today: TODAY }), /expired/);
  assert.throws(() => verify({ ecosystem: "npm", report: npmReport(), config: { version: 1, exceptions: [{ ...reviewedNpmException, reviewed_at: "2026-09-24" }] }, scannerExit: 1, today: TODAY }), /future reviewed_at/);
  assert.throws(() => verify({ ecosystem: "npm", report: npmReport(), config: { version: 1, exceptions: [{ ...reviewedNpmException, expires: "2026-09-22" }] }, scannerExit: 1, today: TODAY }), /on or before/);
  assert.throws(() => verify({ ecosystem: "npm", report: npmReport(), config: { version: 1, exceptions: [{ ...reviewedNpmException, unchecked: true }] }, scannerExit: 1, today: TODAY }), /unsupported fields/);
  assert.throws(() => verify({ ecosystem: "npm", report: npmReport({}), config: { version: 1, exceptions: [reviewedNpmException] }, scannerExit: 0, today: TODAY }), /unused/);
});
test("requires scanner status to agree with complete findings", () => {
  assert.throws(() => verify({ ecosystem: "rustsec", report: rustsecReport([]), config: { version: 1, exceptions: [] }, scannerExit: 1, today: TODAY }), /expected 0/);
  assert.throws(() => verify({ ecosystem: "npm", report: npmReport(), config: { version: 1, exceptions: [reviewedNpmException] }, scannerExit: 0, today: TODAY }), /expected 1/);
  assert.throws(() => verify({ ecosystem: "npm", report: npmReport(), config: { version: 1, exceptions: [reviewedNpmException] }, scannerExit: 2, today: TODAY }), /expected 1/);
});
