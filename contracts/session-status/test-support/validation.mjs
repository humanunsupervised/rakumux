import { Buffer } from "node:buffer";
import { readFileSync } from "node:fs";
import { TextDecoder } from "node:util";
import Ajv2020 from "ajv/dist/2020.js";

/** Canonical schema documents, loaded locally without resolving network references. */
export const schemas = Object.freeze(
  Object.fromEntries(
    ["common", "request", "success", "error"].map((name) => [
      name,
      JSON.parse(readFileSync(new URL(`../${name}.schema.json`, import.meta.url), "utf8")),
    ]),
  ),
);

const ajv = new Ajv2020({
  strict: true,
  allErrors: true,
  ownProperties: true,
  unicodeRegExp: true,
  coerceTypes: false,
  removeAdditional: false,
  useDefaults: false,
});

ajv.addKeyword({
  keyword: "maxUtf8Bytes",
  type: "string",
  schemaType: "number",
  metaSchema: { type: "integer", minimum: 0 },
  validate: (limit, value) => Buffer.byteLength(value, "utf8") <= limit,
  errors: false,
});

// The raw-body parser, not an object-level validator, enforces this annotation.
ajv.addKeyword({
  keyword: "x-maxBodyBytes",
  schemaType: "number",
  metaSchema: { type: "integer", minimum: 0 },
  valid: true,
});

ajv.addSchema(schemas.common);

/** Development-only, non-mutating schema validators; AJV is not a product runtime. */
export const validators = Object.freeze({
  request: ajv.compile(schemas.request),
  success: ajv.compile(schemas.success),
  error: ajv.compile(schemas.error),
});

const utf8Decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/**
 * Validate original request bytes before using their decoded value.
 * Returns static local failure reasons, never input-bearing wire diagnostics.
 * Production HTTP adapters must also stop reading at the cap, before buffering.
 */
export function validateRequestBody(body) {
  if (!(body instanceof Uint8Array)) {
    throw new TypeError("Request body must be original UTF-8 bytes.");
  }

  if (body.byteLength > schemas.request["x-maxBodyBytes"]) {
    return { valid: false, reason: "body_too_large" };
  }

  let json;
  try {
    json = utf8Decoder.decode(body);
  } catch {
    return { valid: false, reason: "invalid_utf8" };
  }

  let value;
  try {
    value = JSON.parse(json);
  } catch {
    return { valid: false, reason: "invalid_json" };
  }

  if (hasDuplicateMembers(json)) {
    return { valid: false, reason: "duplicate_member" };
  }

  if (!validators.request(value)) {
    return { valid: false, reason: "invalid_request" };
  }

  return { valid: true, value };
}

/**
 * Check both message schemas and exact request/response correlation.
 * Local account, consent, terminal and source generations remain separate checks.
 */
export function validateSuccessForRequest(request, response) {
  return (
    validators.request(request) &&
    validators.success(response) &&
    request.request_id === response.request_id
  );
}

// JSON.parse has already checked syntax. Scan member tokens to reject duplicate
// decoded names (including escaped equivalents) without another parser dependency.
function hasDuplicateMembers(json) {
  const containers = [];

  for (let index = 0; index < json.length; index += 1) {
    const character = json[index];
    if (character === "{" || character === "[") {
      containers.push(character === "{" ? new Set() : null);
    } else if (character === "}" || character === "]") {
      containers.pop();
    } else if (character === '"') {
      const end = stringEnd(json, index);
      let next = end + 1;
      while (/[ \t\r\n]/u.test(json[next] ?? "")) next += 1;

      if (json[next] === ":") {
        const name = JSON.parse(json.slice(index, end + 1));
        const names = containers.at(-1);
        if (names.has(name)) return true;
        names.add(name);
      }

      index = end;
    }
  }

  return false;
}

function stringEnd(json, start) {
  for (let index = start + 1; index < json.length; index += 1) {
    if (json[index] === "\\") index += 1;
    else if (json[index] === '"') return index;
  }
  throw new SyntaxError("Expected a complete JSON string.");
}
