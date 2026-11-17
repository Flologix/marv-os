/**
 * Images the person pastes or drops into the composer. The page sends the bytes (`paste_image`), Marv saves them under
 * its `pasted/` folder and answers with the path (`image_pasted`), and the path goes into the message: Marv attaches
 * every image file a message names, so a pasted screenshot and a typed path work the same way.
 */
export type PastedImage = { mediaType: string; data: string };

/** What models take: Marv refuses anything else before it's sent. */
export const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];
/** The providers' per-image limit (Anthropic's 5 MB). */
export const MAX_IMAGE_BYTES = 5 * 1024 * 1024;

/** The first image among pasted or dropped files, if any. */
export function firstImage(files: ArrayLike<File> | null | undefined): File | null {
  if (!files) return null;
  for (let i = 0; i < files.length; i++) {
    if (IMAGE_TYPES.includes(files[i].type)) return files[i];
  }
  return null;
}

/** Why `file` can't be pasted, or null when it can. */
export function refusal(file: { type: string; size: number }): string | null {
  if (!IMAGE_TYPES.includes(file.type)) return "Only PNG, JPEG, GIF and WebP images can be pasted.";
  if (file.size > MAX_IMAGE_BYTES) return `That image is ${(file.size / 1048576).toFixed(1)} MB: models take at most 5 MB.`;
  return null;
}

/** `file` as base64, for `paste_image`. */
export async function readImage(file: Blob): Promise<PastedImage> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return { mediaType: file.type, data: btoa(binary) };
}

/** `text` with `path` put in at `at`, a space either side unless it starts or ends a line; where the caret goes. */
export function withPath(text: string, at: number, path: string): { text: string; caret: number } {
  const at_ = Math.max(0, Math.min(at, text.length));
  const before = text.slice(0, at_);
  const after = text.slice(at_);
  const lead = before === "" || /\s$/.test(before) ? "" : " ";
  const inserted = `${lead}${path} `;
  return { text: before + inserted + (after.startsWith(" ") ? after.slice(1) : after), caret: before.length + inserted.length };
}
