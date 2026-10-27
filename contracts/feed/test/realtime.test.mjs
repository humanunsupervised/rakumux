import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/realtime.json", import.meta.url), "utf8"));

test("realtime tokens validate: a TLS WebSocket, a header-safe token, an expiry", () => {
  for (const value of fixtures.realtime.valid) assert.equal(validators.realtime(value), true, JSON.stringify(value));
  for (const { note, value } of fixtures.realtime.invalid) assert.equal(validators.realtime(value), false, note);
});

test("a poke says nothing but to read", () => {
  for (const value of fixtures.poke.valid) assert.equal(validators.poke(value), true, JSON.stringify(value));
  for (const { note, value } of fixtures.poke.invalid) assert.equal(validators.poke(value), false, note);
});
