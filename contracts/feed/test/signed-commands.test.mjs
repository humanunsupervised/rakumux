import assert from "node:assert/strict";
import { test } from "node:test";
import { validators } from "../test-support/validation.mjs";

const b64 = (bytes) => Buffer.from(bytes).toString("base64url");
const command = b64(JSON.stringify({ v: 1, kind: "delete_session", machine: "01K89ZQ4D2F6H8J0K2M4N6P8R1", id: "01K8A2B3C4D5E6F7G8H9J0K1M2", at: 1761220000000, expires: 1761220600000, session: "01K89ZQ4D2F6H8J0K2M4N6P8R0" }));
const passkey = { device: b64([1, 2, 3, 4]), command, signature: b64(new Uint8Array(70)), authenticator_data: b64(new Uint8Array(37)), client_data: b64("{}") };
const phone = { device: b64([5, 6]), command, signature: b64(new Uint8Array(71)) };
const deletion = { id: "01K8A2B3C4D5E6F7G8H9J0K1M2", machine: "studio", session: "01K89ZQ4D2F6H8J0K2M4N6P8R0", status: "pending", created_at: 1761220000000, status_at: 1761220000000 };
const point = b64(Uint8Array.from({ length: 65 }, (_, i) => (i === 0 ? 4 : i)));
const device = { key_id: b64([1, 2, 3, 4]), format: "webauthn", public_key: point, name: "MacBook" };
const proposal = { id: "01K8A2B3C4D5E6F7G8H9J0K1M3", pairing: "K7QM", device, tag: b64(new Uint8Array(32)) };

test("requests carry the command a paired device signed", () => {
  assert.equal(validators.sessionDeletion({ version: 1, session_deletion: { ...deletion, signed: passkey } }), true);
  assert.equal(validators.sessionDeletion({ version: 1, session_deletion: { ...deletion, signed: phone } }), true);
  const { client_data, ...half } = passkey;
  assert.equal(validators.sessionDeletion({ version: 1, session_deletion: { ...deletion, signed: half } }), false, "a passkey's data goes together");
  assert.equal(validators.sessionDeletion({ version: 1, session_deletion: { ...deletion, signed: { ...phone, signature: "not base64!" } } }), false);
});

test("a machine refuses a command for its signature", () => {
  for (const failure of ["unsigned", "unknown_device", "bad_signature", "expired"]) {
    assert.equal(validators.sessionDeletionStatus({ version: 1, id: deletion.id, machine: "studio", status: "failed", failure }), true, failure);
    assert.equal(validators.replyStatus({ version: 1, id: deletion.id, machine: "studio", status: "failed", failure }), true, failure);
  }
});

test("replies written on a machine may be signed", () => {
  assert.equal(validators.replyUpload({ version: 1, id: deletion.id, entry: deletion.session, text: "go on", from: "studio", signed: phone }), true);
});

test("a session list names its paired devices and its id", () => {
  const list = { version: 1, machine: "studio", sessions: [], at: 1761220000000, capabilities: ["signed_commands"], paired_devices: [{ key_id: device.key_id, format: "p256", name: "iPhone" }], machine_id: "01K89ZQ4D2F6H8J0K2M4N6P8R1" };
  assert.equal(validators.sessionsUpload(list), true);
  assert.equal(validators.sessionsUpload({ ...list, paired_devices: [{ ...device }] }), false, "never a public key");
});

test("a pairing goes by the code's first group only", () => {
  assert.equal(validators.pairingOpen({ version: 1, machine: "studio", pairing: "K7QM", expires_at: 1761220300000 }), true);
  assert.equal(validators.pairingOpen({ version: 1, machine: "studio", pairing: "K7QM4XPA", expires_at: 1761220300000 }), false);
  assert.equal(validators.pairingOpen({ version: 1, machine: "studio", pairing: "K7QO", expires_at: 1761220300000 }), false);
  assert.equal(validators.pairing({ version: 1, pairing: "K7QM", expires_at: 1761220300000 }), true);
});

test("a device proposes its key with the code's tag", () => {
  assert.equal(validators.pairingPropose({ version: 1, device, tag: proposal.tag }), true);
  assert.equal(validators.pairingPropose({ version: 1, device: { ...device, name: "two\nlines" }, tag: proposal.tag }), false);
  assert.equal(validators.pairingPropose({ version: 1, device: { ...device, public_key: "AAAA" }, tag: proposal.tag }), false);
  assert.equal(validators.replies({ version: 1, replies: [], latest_seq: 0, more: false, pairings: [proposal] }), true);
  assert.equal(validators.replies({ version: 1, replies: [], latest_seq: 0, more: false, pairings: [proposal, proposal] }), false);
});

test("a machine reports a proposal paired or why not", () => {
  const status = { version: 1, machine: "studio", pairing: "K7QM", proposal: proposal.id };
  assert.equal(validators.pairingStatus({ ...status, status: "paired" }), true);
  assert.equal(validators.pairingStatus({ ...status, status: "failed", failure: "wrong_code" }), true);
  assert.equal(validators.pairingStatus({ ...status, status: "failed" }), false);
  assert.equal(validators.pairingProposal({ version: 1, id: proposal.id, pairing: "K7QM", machine: "studio", status: "pending" }), true);
  assert.equal(validators.pairingProposal({ version: 1, id: proposal.id, pairing: "K7QM", machine: "studio", status: "failed", failure: "refused" }), true);
});

test("pairing errors are static", () => {
  assert.equal(validators.error({ error: { code: "pairing_taken", message: "Another pairing uses this code." } }), true);
  assert.equal(validators.error({ error: { code: "pairing_closed", message: "No pairing waits for this code." } }), true);
});
