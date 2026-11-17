// `npm test`: plain Node (node:test), the TypeScript stripped.
import assert from "node:assert/strict";
import { test } from "node:test";
import { answerText, pickedIn, splitChoices, startAt } from "./choices.ts";

const REPLY = `Backend is live. My take:

\`\`\`choices
? Which fix should we make for the slow economy?
A) Retune the upgrade costs only (recommended) — keeps the clean-and-regrow loop
B) Slow the regrowth — longer waits
C) Both
\`\`\`
`;

test("a choices block becomes a question, its text leaves the reply", () => {
  const { text, questions } = splitChoices(REPLY);
  assert.equal(text, "Backend is live. My take:");
  assert.equal(questions.length, 1);
  const q = questions[0];
  assert.equal(q.question, "Which fix should we make for the slow economy?");
  assert.deepEqual(
    q.options.map((o) => [o.key, o.label, o.why, o.recommended]),
    [
      ["A", "Retune the upgrade costs only", "keeps the clean-and-regrow loop", true],
      ["B", "Slow the regrowth", "longer waits", false],
      ["C", "Both", undefined, false],
    ],
  );
  assert.equal(startAt(q), 0);
});

test("the answer names the pick, and the card finds it in the next message", () => {
  const { questions } = splitChoices(REPLY);
  const answer = answerText(questions, [1]);
  assert.equal(answer, "B) Slow the regrowth");
  assert.equal(pickedIn(questions[0], answer), 1);
  assert.equal(pickedIn(questions[0], "something else"), -1);
});

test("several questions answer on one line each; only the first recommended counts", () => {
  const two = "```choices\n? Engine?\n1. Three.js (recommended)\n2. Babylon (recommended)\n```\n\n```choices\n? Tests?\nA) Yes\nB) No\n```";
  const { text, questions } = splitChoices(two);
  assert.equal(text, "");
  assert.deepEqual(
    questions[0].options.map((o) => o.recommended),
    [true, false],
  );
  assert.equal(startAt(questions[1]), 0);
  assert.equal(answerText(questions, [0, 1]), "Engine? → 1) Three.js\nTests? → B) No");
});

test("a block with one option, or other code, stays as it was", () => {
  const odd = "Look:\n\n```choices\n? Only one?\nA) This\n```\n\n```js\nA) not a choice\n```";
  assert.deepEqual(splitChoices(odd), { text: odd, questions: [] });
});
