import { Buffer } from "node:buffer";
import { readFileSync } from "node:fs";
import Ajv2020 from "ajv/dist/2020.js";

/** Canonical schema documents, loaded locally without resolving network references. */
export const schemas = Object.freeze(
  Object.fromEntries(
    ["common", "upload", "accepted", "page", "error"].map((name) => [
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

// The raw-body reader, not an object-level validator, enforces this annotation.
ajv.addKeyword({
  keyword: "x-maxBodyBytes",
  schemaType: "number",
  metaSchema: { type: "integer", minimum: 0 },
  valid: true,
});

ajv.addSchema(schemas.common);

/** Development-only, non-mutating schema validators; AJV is not a product runtime. */
export const validators = Object.freeze({
  upload: ajv.compile(schemas.upload),
  accepted: ajv.compile(schemas.accepted),
  page: ajv.compile(schemas.page),
  error: ajv.compile(schemas.error),
});
