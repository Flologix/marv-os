import assert from "node:assert/strict";
import { test } from "node:test";
import { APPS, PROVIDERS, clientsFor, fillIn, redirectUri } from "./apps.ts";

test("an app's address and scope take the fields typed for it", () => {
  const outlook = APPS.find((a) => a.id === "outlook-mail")!.connect;
  assert.equal(outlook.kind, "oauth");
  if (outlook.kind !== "oauth") return;
  const url = fillIn(outlook.url, { TENANT_ID: " 1234 " });
  assert.equal(url, "https://agent365.svc.cloud.microsoft/agents/tenants/1234/servers/mcp_MailTools");
  assert.ok(!fillIn(outlook.scope!, { TENANT_ID: "1234" }).includes("${"));
  assert.equal(fillIn("a ${MISSING}", {}), "a ${MISSING}");
});

test("every app signs in over https, and each provider has its own fixed loopback port", () => {
  for (const app of APPS) if (app.connect.kind === "oauth") assert.match(app.connect.url, /^https:\/\//, app.id);
  const ports = Object.values(PROVIDERS).map((p) => p.callbackPort);
  assert.equal(new Set(ports).size, ports.length);
  assert.equal(redirectUri("google"), "http://127.0.0.1:53682/callback");
  assert.equal(new Set(APPS.map((a) => a.id)).size, APPS.length);
});

test("a second Google app offers the first one's client, never another provider's", () => {
  const mine = [
    { name: "gmail", hasClient: true },
    { name: "notion", hasClient: false },
    { name: "slack", hasClient: true },
    { name: "google-drive", hasClient: false },
  ];
  assert.deepEqual(clientsFor("google", mine), ["gmail"]);
  assert.deepEqual(clientsFor("slack", mine), ["slack"]);
  assert.deepEqual(clientsFor("microsoft", mine), []);
});
