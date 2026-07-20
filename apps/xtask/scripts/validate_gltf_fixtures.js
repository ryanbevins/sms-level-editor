// Runs the official Khronos glTF Validator package over project-authored fixtures.
// Usage: node validate_gltf_fixtures.js <validator-module> <valid-fixture-root>

"use strict";

const fs = require("fs");
const path = require("path");

const [validatorModule, fixtureRootArgument] = process.argv.slice(2);
if (!validatorModule || !fixtureRootArgument) {
  console.error("expected validator module path and valid fixture root");
  process.exit(2);
}

const validator = require(path.resolve(validatorModule));
const fixtureRoot = fs.realpathSync(fixtureRootArgument);

function fixtureFiles(directory) {
  return fs
    .readdirSync(directory, { withFileTypes: true })
    .sort((left, right) => left.name.localeCompare(right.name))
    .flatMap((entry) => {
      const candidate = path.join(directory, entry.name);
      if (entry.isDirectory()) return fixtureFiles(candidate);
      return /\.(gltf|glb)$/i.test(entry.name) ? [candidate] : [];
    });
}

function loadExternalResource(assetPath, uri) {
  if (/^[a-z][a-z0-9+.-]*:/i.test(uri)) {
    return Promise.reject(`network and absolute URIs are forbidden: ${uri}`);
  }
  const decoded = decodeURIComponent(uri);
  const resolved = path.resolve(path.dirname(assetPath), decoded);
  const relative = path.relative(fixtureRoot, resolved);
  if (relative.startsWith("..") || path.isAbsolute(relative)) {
    return Promise.reject(`resource escapes fixture root: ${uri}`);
  }
  return fs.promises.readFile(resolved).then((bytes) => new Uint8Array(bytes));
}

async function main() {
  const reports = [];
  let errorCount = 0;
  for (const assetPath of fixtureFiles(fixtureRoot)) {
    const bytes = fs.readFileSync(assetPath);
    const report = await validator.validateBytes(new Uint8Array(bytes), {
      uri: path.relative(fixtureRoot, assetPath).replaceAll(path.sep, "/"),
      format: path.extname(assetPath).slice(1).toLowerCase(),
      maxIssues: 0,
      writeTimestamp: false,
      externalResourceFunction: (uri) => loadExternalResource(assetPath, uri),
    });
    const issues = report.issues ?? {};
    errorCount += issues.numErrors ?? 0;
    reports.push({
      asset: path.relative(fixtureRoot, assetPath).replaceAll(path.sep, "/"),
      errors: issues.numErrors ?? 0,
      warnings: issues.numWarnings ?? 0,
      infos: issues.numInfos ?? 0,
      messages: issues.messages ?? [],
    });
  }
  console.log(
    JSON.stringify(
      { validatorVersion: validator.version(), errorCount, reports },
      null,
      2,
    ),
  );
  if (errorCount !== 0) process.exitCode = 1;
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack : String(error));
  process.exitCode = 2;
});
