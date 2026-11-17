// `npm test`: plain Node (node:test), the TypeScript stripped.
import assert from "node:assert/strict";
import { test } from "node:test";
import { MAX_IMAGE_BYTES, firstImage, readImage, refusal, withPath } from "./images.ts";

test("a pasted path is spaced from the words around it, and the caret goes after it", () => {
  assert.deepEqual(withPath("", 0, "/p/a.png"), { text: "/p/a.png ", caret: 9 });
  assert.deepEqual(withPath("look at", 7, "/p/a.png"), { text: "look at /p/a.png ", caret: 17 });
  assert.deepEqual(withPath("look at  please", 8, "/p/a.png"), { text: "look at /p/a.png please", caret: 17 });
  assert.deepEqual(withPath("ab", 1, "/x.png"), { text: "a /x.png b", caret: 9 });
});

test("only an image a model takes, at most 5 MB, can be pasted", () => {
  assert.equal(refusal({ type: "image/png", size: 10 }), null);
  assert.match(refusal({ type: "image/tiff", size: 10 }) ?? "", /PNG, JPEG, GIF and WebP/);
  assert.match(refusal({ type: "image/jpeg", size: MAX_IMAGE_BYTES + 1 }) ?? "", /at most 5 MB/);
});

test("the first image among the files is the one pasted", async () => {
  const text = new File(["hi"], "a.txt", { type: "text/plain" });
  const png = new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], "s.png", { type: "image/png" });
  assert.equal(firstImage([text]), null);
  assert.equal(firstImage([text, png]), png);
  assert.deepEqual(await readImage(png), { mediaType: "image/png", data: "iVBORw==" });
});
