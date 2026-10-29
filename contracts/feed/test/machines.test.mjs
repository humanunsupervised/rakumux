import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { validators } from "../test-support/validation.mjs";

const fixtures = JSON.parse(readFileSync(new URL("../fixtures/machines.json", import.meta.url), "utf8"));

function check(name, validate) {
  for (const value of fixtures[name].valid) assert.equal(validate(value), true, JSON.stringify(value));
  for (const { note, value } of fixtures[name].invalid) assert.equal(validate(value), false, note);
}

test("an enrol names the machine and wakes it only at a Sprite's URL", () => check("enrol", validators.machineEnrol));
test("a credential is a machine secret, given once", () => check("credential", validators.machineCredential));
test("the machines list never carries a secret", () => check("credentials", validators.machineCredentials));
test("an access token is a machine's, never a realtime token", () => check("token", validators.machineToken));
