/**
 * A reply's `choices` blocks (marv-web's CHOICES_PROMPT): the question after `?`, then one option per line,
 * `A) label (recommended) — why`. The chat shows them as buttons; the pick goes back as the person's next message.
 */
export type Choice = { key: string; label: string; why?: string; recommended: boolean };
export type Question = { question: string; options: Choice[] };

const FENCE = /^```choices[^\n]*\n([\s\S]*?)^```[ \t]*$/gm;
const OPTION = /^\s*(?:[-*]\s*)?([A-Za-z]|\d{1,2})[).:]\s+(.+)$/;
const RECOMMENDED = /\s*[([]\s*recommended\s*[)\]]\s*/i;

/** The reply's text without its choices blocks, and the questions they hold (a block with under two options stays text). */
export function splitChoices(text: string): { text: string; questions: Question[] } {
  const questions: Question[] = [];
  const rest = text.replace(FENCE, (whole, body: string) => {
    const q = parseBlock(body);
    if (!q) return whole;
    questions.push(q);
    return "";
  });
  return { text: questions.length ? rest.replace(/\n{3,}/g, "\n\n").trim() : text, questions };
}

function parseBlock(body: string): Question | null {
  let question = "";
  const options: Choice[] = [];
  for (const line of body.split("\n")) {
    const t = line.trim();
    if (!t) continue;
    if (t.startsWith("?")) {
      question = t.slice(1).trim();
      continue;
    }
    const m = OPTION.exec(t);
    if (!m) {
      if (!options.length && !question) question = t;
      continue;
    }
    let rest = m[2];
    const recommended = RECOMMENDED.test(rest);
    rest = rest.replace(RECOMMENDED, " ").trim();
    const [label, ...why] = rest.split(/\s+[—–]\s+|\s+-\s+/);
    options.push({ key: m[1].toUpperCase(), label: label.trim(), why: why.join(" — ").trim() || undefined, recommended });
  }
  if (options.length < 2) return null;
  // At most one recommended: the first marked.
  const first = options.findIndex((o) => o.recommended);
  options.forEach((o, i) => (o.recommended = i === first));
  return { question, options };
}

/** Where the highlight starts: the recommended option, else the first. */
export const startAt = (q: Question) => Math.max(0, q.options.findIndex((o) => o.recommended));

/** The message a pick sends: `A) label`, or one line per question when a reply asked several. */
export function answerText(questions: Question[], picks: number[]): string {
  const one = (q: Question, i: number) => `${q.options[i].key}) ${q.options[i].label}`;
  if (questions.length === 1) return one(questions[0], picks[0]);
  return questions.map((q, n) => `${q.question || `Question ${n + 1}`} → ${one(q, picks[n])}`).join("\n");
}

/** Which option a sent answer picked, if it was one of these (the card shows it after). */
export function pickedIn(q: Question, answer: string | undefined): number {
  if (!answer) return -1;
  return q.options.findIndex((o) => answer.includes(`${o.key}) ${o.label}`));
}
