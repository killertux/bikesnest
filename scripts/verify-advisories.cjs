#!/usr/bin/env node
// Fail closed: a scanner report is accepted only when every current advisory
// has one current, reviewed exception and every exception is still exercised.
const fs = require("node:fs");

const DATE = /^\d{4}-\d{2}-\d{2}$/;
const ECOSYSTEMS = new Set(["npm", "rustsec"]);
const EXCEPTION_FIELDS = new Set(["ecosystem", "advisory", "owner", "reviewed_by", "reviewed_at", "rationale", "expires"]);

function fail(message) {
  throw new Error(message);
}

function parseArgs(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (!key?.startsWith("--") || value === undefined || options[key] !== undefined) {
      fail("usage: verify-advisories --ecosystem <npm|rustsec> --report <json> --scanner-exit <code> [--config <json>] [--today YYYY-MM-DD]");
    }
    options[key] = value;
  }
  for (const required of ["--ecosystem", "--report", "--scanner-exit"]) {
    if (options[required] === undefined) fail(`missing ${required}`);
  }
  if (!ECOSYSTEMS.has(options["--ecosystem"])) fail("unknown ecosystem");
  if (!/^\d+$/.test(options["--scanner-exit"])) fail("scanner exit must be a non-negative integer");
  return options;
}

function readJson(path, label) {
  let text;
  try {
    text = fs.readFileSync(path, "utf8");
  } catch (error) {
    fail(`cannot read ${label}: ${error.message}`);
  }
  try {
    return JSON.parse(text);
  } catch (error) {
    fail(`cannot parse ${label}: ${error.message}`);
  }
}

function isIsoDate(value) {
  if (typeof value !== "string" || !DATE.test(value)) return false;
  const date = new Date(`${value}T00:00:00Z`);
  return !Number.isNaN(date.valueOf()) && date.toISOString().slice(0, 10) === value;
}

function validateExceptions(config, today) {
  if (!config || typeof config !== "object" || Array.isArray(config)) {
    fail("exception config must be an object");
  }
  if (config.version !== 1 || !Array.isArray(config.exceptions)) {
    fail("exception config must contain version 1 and an exceptions array");
  }
  if (Object.keys(config).some((key) => key !== "version" && key !== "exceptions")) {
    fail("exception config has unsupported fields");
  }
  const seen = new Set();
  const exceptions = new Map();
  for (const entry of config.exceptions) {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) fail("each exception must be an object");
    if (Object.keys(entry).some((key) => !EXCEPTION_FIELDS.has(key))) fail("exception has unsupported fields");
    for (const key of ["ecosystem", "advisory", "owner", "reviewed_by", "reviewed_at", "rationale", "expires"]) {
      if (typeof entry[key] !== "string" || entry[key].trim() === "") fail(`exception has an empty ${key}`);
    }
    if (!ECOSYSTEMS.has(entry.ecosystem)) fail(`exception has unsupported ecosystem ${entry.ecosystem}`);
    if (!isIsoDate(entry.reviewed_at) || !isIsoDate(entry.expires)) fail("exception reviewed_at and expires must be ISO calendar dates");
    if (entry.reviewed_at > today) fail(`exception ${entry.ecosystem}:${entry.advisory} has a future reviewed_at date`);
    if (entry.expires <= entry.reviewed_at) fail(`exception ${entry.ecosystem}:${entry.advisory} expires on or before reviewed_at`);
    if (entry.expires <= today) fail(`exception ${entry.ecosystem}:${entry.advisory} is expired on ${entry.expires}`);
    if (entry.rationale.trim().length < 16) fail(`exception ${entry.ecosystem}:${entry.advisory} rationale is too short`);
    const key = `${entry.ecosystem}:${entry.advisory}`;
    if (seen.has(key)) fail(`duplicate exception ${key}`);
    seen.add(key);
    exceptions.set(key, entry);
  }
  return exceptions;
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value) && Object.getPrototypeOf(value) === Object.prototype;
}

function isNonnegativeInteger(value) {
  return Number.isInteger(value) && value >= 0;
}

function ghsaFromUrl(value) {
  if (typeof value !== "string") fail("npm advisory URL must be a string");
  const match = /^https:\/\/github\.com\/advisories\/(GHSA-[23456789CFGHJMPQRVWX]{4}-[23456789CFGHJMPQRVWX]{4}-[23456789CFGHJMPQRVWX]{4})(?:[/?#].*)?$/i.exec(value);
  if (!match) fail("npm advisory object does not contain an exact GHSA URL");
  return match[1].toUpperCase();
}

function advisoriesFromNpm(report) {
  if (!isPlainObject(report) || report.auditReportVersion !== 2 || !isPlainObject(report.vulnerabilities)) {
    fail("npm audit did not return an auditReportVersion 2 vulnerabilities report");
  }
  if (report.error) fail("npm audit reported an error instead of an advisory result");
  const summary = report.metadata?.vulnerabilities;
  const severities = ["info", "low", "moderate", "high", "critical"];
  if (!isPlainObject(summary) || !isNonnegativeInteger(summary.total) || severities.some((severity) => !isNonnegativeInteger(summary[severity]))) {
    fail("npm audit metadata.vulnerabilities must contain non-negative severity counts and total");
  }
  if (severities.reduce((sum, severity) => sum + summary[severity], 0) !== summary.total) {
    fail("npm audit metadata severity counts disagree with total");
  }
  const entries = report.vulnerabilities;
  const names = Object.keys(entries);
  if (names.length !== summary.total) fail("npm audit vulnerability inventory disagrees with metadata total");
  const severityInventory = Object.fromEntries(severities.map((severity) => [severity, 0]));
  for (const entry of Object.values(entries)) {
    if (!isPlainObject(entry) || !severities.includes(entry.severity)) fail("npm audit vulnerability inventory has an unsupported severity");
    severityInventory[entry.severity] += 1;
  }
  if (severities.some((severity) => severityInventory[severity] !== summary[severity])) {
    fail("npm audit metadata severity counts disagree with vulnerability inventory");
  }
  const resolved = new Map();
  const visiting = new Set();
  function resolve(name) {
    if (resolved.has(name)) return resolved.get(name);
    if (visiting.has(name)) fail(`npm audit vulnerability reference cycle at ${name}`);
    const entry = entries[name];
    if (!isPlainObject(entry) || entry.name !== name || !severities.includes(entry.severity) || typeof entry.isDirect !== "boolean" || !Array.isArray(entry.via) || entry.via.length === 0 || !Array.isArray(entry.effects) || typeof entry.range !== "string" || !Array.isArray(entry.nodes)) {
      fail(`npm audit vulnerability entry ${name} has an unsupported shape`);
    }
    visiting.add(name);
    const ids = new Set();
    for (const via of entry.via) {
      if (typeof via === "string") {
        if (!Object.prototype.hasOwnProperty.call(entries, via)) fail(`npm audit vulnerability ${name} references missing ${via}`);
        for (const id of resolve(via)) ids.add(id);
      } else if (isPlainObject(via)) {
        ids.add(ghsaFromUrl(via.url));
      } else {
        fail(`npm audit vulnerability ${name} has an unsupported via entry`);
      }
    }
    visiting.delete(name);
    if (ids.size === 0) fail(`npm audit vulnerability ${name} does not resolve to a GHSA`);
    resolved.set(name, ids);
    return ids;
  }
  const advisories = new Set();
  for (const name of names) for (const id of resolve(name)) advisories.add(id);
  return advisories;
}

function advisoriesFromRustSec(report) {
  const list = report?.vulnerabilities?.list;
  const summary = report?.vulnerabilities;
  if (!isPlainObject(report) || !isPlainObject(summary) || !Array.isArray(list) || typeof summary.found !== "boolean" || !isNonnegativeInteger(summary.count) || summary.count !== list.length) {
    fail("cargo audit did not return a RustSec vulnerabilities report");
  }
  const advisories = new Set();
  for (const finding of list) {
    if (!isPlainObject(finding) || !isPlainObject(finding.advisory)) fail("cargo audit reported a malformed vulnerability finding");
    const id = finding?.advisory?.id;
    if (typeof id !== "string" || !/^RUSTSEC-\d{4}-\d{4}$/.test(id)) fail("cargo audit reported a vulnerability without an exact RustSec advisory id");
    advisories.add(id);
  }
  if (report.vulnerabilities.found !== (advisories.size > 0)) fail("cargo audit vulnerability count disagrees with its report");
  return advisories;
}

function verify({ ecosystem, report, config, scannerExit, today }) {
  const exceptions = validateExceptions(config, today);
  const current = ecosystem === "npm" ? advisoriesFromNpm(report) : advisoriesFromRustSec(report);
  const currentKeys = new Set([...current].map((id) => `${ecosystem}:${id}`));
  const unused = [...exceptions.keys()].filter((key) => key.startsWith(`${ecosystem}:`) && !currentKeys.has(key));
  if (unused.length) fail(`unused advisory exception(s): ${unused.join(", ")}`);
  const expectedScannerExit = current.size === 0 ? 0 : 1;
  if (scannerExit !== expectedScannerExit) {
    fail(`the ${ecosystem} scanner exit ${scannerExit} disagrees with ${current.size} advisory finding(s); expected ${expectedScannerExit}`);
  }
  const unreviewed = [...currentKeys].filter((key) => !exceptions.has(key));
  if (unreviewed.length) fail(`unreviewed advisory finding(s): ${unreviewed.join(", ")}`);
  return [...current].sort();
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const today = options["--today"] || new Date().toISOString().slice(0, 10);
  if (!isIsoDate(today)) fail("today must be an ISO calendar date");
  const ecosystem = options["--ecosystem"];
  const findings = verify({
    ecosystem,
    report: readJson(options["--report"], `${ecosystem} report`),
    config: readJson(options["--config"] || "ci/advisory-exceptions.json", "exception config"),
    scannerExit: Number(options["--scanner-exit"]),
    today,
  });
  console.log(`${ecosystem}: ${findings.length} advisory finding(s); all reviewed exceptions are current`);
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    console.error(`advisory verification failed: ${error.message}`);
    process.exitCode = 1;
  }
}

module.exports = { advisoriesFromNpm, advisoriesFromRustSec, validateExceptions, verify };
