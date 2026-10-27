import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  schemas,
  validators,
  validateRequestBody,
  validateSuccessForRequest,
} from "../test-support/validation.mjs";

function fixtures(name) {
  return JSON.parse(readFileSync(new URL(`../fixtures/${name}.json`, import.meta.url), "utf8"));
}

const request = fixtures("requests")[0].value;
const success = fixtures("successes")[0].value;
const error = fixtures("errors")[0].value;

for (const [schema, filename] of [
  ["request", "requests"],
  ["success", "successes"],
  ["error", "errors"],
]) {
  for (const fixture of fixtures(filename)) {
    test(`${schema}: ${fixture.name}`, () => {
      const original = structuredClone(fixture.value);
      assert.equal(validators[schema](fixture.value), fixture.valid);
      assert.deepEqual(fixture.value, original, "validation must not coerce, strip or mutate");
    });
  }
}

for (const fixture of fixtures("boundaries")) {
  test(`boundary: ${fixture.name}`, () => {
    const text = fixture.text.repeat(fixture.count) + fixture.suffix;
    const value = boundaryValue(fixture, text);
    assert.equal(validators[fixture.schema](value), fixture.valid);

    if (!fixture.valid && fixture.schema === "request") {
      assert.ok(validators.request.errors.some(({ keyword }) => keyword === "maxUtf8Bytes"));
    }
    if (fixture.name === "error_message_over_bytes") {
      assert.ok(validators.error.errors.some(({ keyword }) => keyword === "maxUtf8Bytes"));
    }
    if (fixture.name === "error_message_over_codepoints") {
      assert.ok(validators.error.errors.some(({ keyword }) => keyword === "maxLength"));
    }
  });
}

function boundaryValue(fixture, text) {
  switch (fixture.schema) {
    case "request":
      return { ...request, context: { ...request.context, [fixture.field]: text } };
    case "success":
      return { ...success, [fixture.field]: text };
    case "error":
      return { error: { ...error.error, [fixture.field]: text } };
    default:
      throw new Error("Unknown boundary fixture schema.");
  }
}

for (const fixture of fixtures("bodies")) {
  test(`raw body: ${fixture.name}`, () => {
    const body = fixture.bytes ? Uint8Array.from(fixture.bytes) : Buffer.from(fixture.body, "utf8");
    const result = validateRequestBody(body);
    assert.equal(result.valid, fixture.valid);
    if (fixture.reason) assert.deepEqual(result, { valid: false, reason: fixture.reason });
  });
}

for (const fixture of fixtures("body-bounds")) {
  test(`raw body: ${fixture.name}`, () => {
    const value = fixture.output_count
      ? { ...request, context: { lifecycle: "working", recent_output: "a".repeat(fixture.output_count) } }
      : request;
    assert.equal(validators.request(value), true, "decoded object alone must not prove a wire limit");

    let json = JSON.stringify(value);
    if (fixture.encoding === "escape_ascii") json = json.replaceAll("a", "\\u0061");
    if (fixture.pad_to_bytes) {
      json += " ".repeat(fixture.pad_to_bytes - Buffer.byteLength(json, "utf8"));
    }

    assert.deepEqual(JSON.parse(json), value, "whitespace and escape spelling must not change context");
    const result = validateRequestBody(Buffer.from(json, "utf8"));
    assert.equal(result.valid, fixture.valid);
    if (fixture.reason) assert.deepEqual(result, { valid: false, reason: fixture.reason });
  });
}

for (const fixture of fixtures("correlation")) {
  test(`correlation: ${fixture.name}`, () => {
    assert.equal(validators.request(fixture.request), true);
    assert.equal(validators.success(fixture.response), true);
    assert.equal(validateSuccessForRequest(fixture.request, fixture.response), fixture.valid);
  });
}

test("correlation: matching IDs cannot bypass either message schema", () => {
  assert.equal(validateSuccessForRequest({ ...request, version: 2 }, success), false);
  assert.equal(validateSuccessForRequest(request, { ...success, status_text: "\n" }), false);
  assert.equal(validateSuccessForRequest(request, { ...success, title: "Tests." }), false);
  assert.equal(validateSuccessForRequest(null, success), false);
  assert.equal(validateSuccessForRequest(request, null), false);
});

for (const lifecycle of schemas.request.properties.context.properties.lifecycle.enum) {
  test(`lifecycle: ${lifecycle}`, () => {
    assert.equal(validators.request({ ...request, context: { ...request.context, lifecycle } }), true);
  });
}

for (const activity_phase of schemas.request.properties.context.properties.activity_phase.enum) {
  test(`activity phase: ${activity_phase}`, () => {
    assert.equal(validators.request({ ...request, context: { ...request.context, activity_phase } }), true);
  });
}

const requestIds = [
  { name: "one_character", value: "a", valid: true },
  { name: "exact_limit", value: "a".repeat(64), valid: true },
  { name: "one_over", value: "a".repeat(65), valid: false },
  { name: "all_permitted_punctuation", value: "A0-_.z", valid: true },
  { name: "empty", value: "", valid: false },
  { name: "leading_punctuation", value: "_synthetic", valid: false },
  { name: "space", value: "synthetic id", valid: false },
  { name: "slash", value: "synthetic/id", valid: false },
  { name: "non_ascii", value: "synthétique", valid: false },
  { name: "trailing_newline", value: "synthetic\n", valid: false },
  { name: "terminal_control", value: "synthetic\u001b", valid: false },
  { name: "not_a_string", value: 1, valid: false },
];

for (const fixture of requestIds) {
  test(`public request ID: ${fixture.name}`, () => {
    assert.equal(validators.request({ ...request, request_id: fixture.value }), fixture.valid);
    assert.equal(validators.success({ ...success, request_id: fixture.value }), fixture.valid);
  });
}

const prohibitedFields = [
  "token", "access_token", "refresh_token", "id_token", "session_token",
  "session_id", "user_id", "organization_id", "customer_id", "billing",
  "path", "cwd", "scope", "database", "provider_url", "model", "tools", "messages",
  "__proto__", "constructor", "prototype",
];

for (const field of prohibitedFields) {
  test(`strict objects reject extra field: ${field}`, () => {
    const extra = { [field]: "synthetic-rejected-value" };
    assert.equal(validators.request({ ...request, ...extra }), false);
    assert.equal(validators.request({ ...request, context: { ...request.context, ...extra } }), false);
    assert.equal(validators.success({ ...success, ...extra }), false);
    assert.equal(validators.error({ ...error, ...extra }), false);
    assert.equal(validators.error({ error: { ...error.error, ...extra } }), false);
  });
}

test("schemas use draft 2020-12 and strict object shapes", () => {
  for (const schema of Object.values(schemas)) {
    assert.equal(schema.$schema, "https://json-schema.org/draft/2020-12/schema");
    assertStrictObjects(schema);
  }
});

function assertStrictObjects(value) {
  if (value === null || typeof value !== "object") return;
  if (value.type === "object") assert.equal(value.additionalProperties, false);
  for (const child of Object.values(value)) assertStrictObjects(child);
}

test("agreed field and body limits are explicit in the schema", () => {
  const context = schemas.request.properties.context.properties;
  assert.equal(context.reported_text.maxUtf8Bytes, 2048);
  assert.equal(context.recent_output.maxUtf8Bytes, 8192);
  assert.equal(schemas.request["x-maxBodyBytes"], 16384);
  assert.equal(schemas.common.$defs.statusText.maxLength, 140);
  assert.equal(schemas.common.$defs.sessionTitle.maxLength, 60);
  assert.equal(schemas.common.$defs.sessionTitle.maxUtf8Bytes, 240);
  assert.equal(schemas.success.properties.title.$ref.endsWith("#/$defs/sessionTitle"), true);
  assert.equal(schemas.success.required.includes("title"), false);
  assert.equal(context.reported_text.maxLength, undefined);
  assert.equal(context.recent_output.maxLength, undefined);
});

test("both context fields can simultaneously reach their byte caps", () => {
  const value = {
    ...request,
    context: { lifecycle: "working", reported_text: "a".repeat(2048), recent_output: "😀".repeat(2048) },
  };
  assert.equal(validators.request(value), true);
  assert.equal(validateRequestBody(Buffer.from(JSON.stringify(value), "utf8")).valid, true);
});

test("all C0, DEL and C1 output controls are rejected", () => {
  const codepoints = [...Array(32).keys(), ...Array.from({ length: 33 }, (_, index) => index + 127)];
  for (const codepoint of codepoints) {
    const text = `Checking${String.fromCodePoint(codepoint)}output`;
    assert.equal(validators.success({ ...success, status_text: text }), false);
    assert.equal(validators.success({ ...success, title: text }), false);
  }
});

test("a title is optional and never required for status text", () => {
  assert.equal(validators.success(success), true);
  assert.equal(validators.success({ ...success, title: "Validation tests" }), true);
});

test("a title ends without a period or whitespace", () => {
  for (const title of ["Tests.", "Tests. ", " Tests", "Tests\u3000", "Tests\u2003", "."]) {
    assert.equal(validators.success({ ...success, title }), false, JSON.stringify(title));
  }
  for (const title of ["Tests!", "v0.1.24", "Tests…", "Tests。", "Fix a.b"]) {
    assert.equal(validators.success({ ...success, title }), true, JSON.stringify(title));
  }
});

test("additional invisible and bidi controls are rejected", () => {
  const codepoints = [
    0x034f, 0x061c, 0x115f, 0x1160, 0x17b4, 0x180e, 0x200c, 0x200e, 0x200f,
    0x202a, 0x202b, 0x202c, 0x202d, 0x2060, 0x2067, 0x2068, 0x2069, 0xfeff,
    0xffa0, 0x1bca0, 0xe0001, 0xe0100,
  ];
  for (const codepoint of codepoints) {
    const text = `Checking${String.fromCodePoint(codepoint)}output`;
    assert.equal(validators.success({ ...success, status_text: text }), false);
    assert.equal(validators.success({ ...success, title: text }), false);
  }
});

test("input controls are rejected even when the other source is meaningful", () => {
  for (const field of ["reported_text", "recent_output"]) {
    const value = { ...request, context: { ...request.context, recent_output: "Checking", [field]: "\u0000" } };
    assert.equal(validators.request(value), false);
  }
});

test("the byte boundary requires original bytes, not an already parsed object", () => {
  assert.throws(() => validateRequestBody(request), TypeError);
  assert.throws(() => validateRequestBody(JSON.stringify(request)), TypeError);
});

test("duplicate-member scanning handles nested arrays and independent objects", () => {
  const bodies = [
    ['{"extra":[{"x":1},{"x":2}]}', "invalid_request"],
    ['{"extra":[{"x":1,"x":2}]}', "duplicate_member"],
    ['{"extra":{"x":1},"other":{"x":2}}', "invalid_request"],
    ['{"extra":{"x":1,"\\u0078":2}}', "duplicate_member"],
  ];
  for (const [body, reason] of bodies) {
    assert.deepEqual(validateRequestBody(Buffer.from(body)), { valid: false, reason });
  }
});
