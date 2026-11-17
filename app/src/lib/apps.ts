// The app gallery: the tools a business runs on, each a remote MCP server you sign in to with OAuth (your browser
// opens the provider's own consent page; Marv never sees a password). Connecting writes the server to ~/.marv/mcp.json
// (yours only) and signs in from the Apps place: no chat needed.
//
// Servers that register clients themselves (Notion, Linear) need nothing typed. Google, Microsoft, Slack and GitHub
// only accept a client registered ahead: Marv's own once it ships one (MARV_CLIENTS), else your organization's, set up
// once by whoever runs your Google Workspace, Microsoft 365 or Slack, and reused by that provider's other apps.

export type Provider = "google" | "microsoft" | "slack" | "github";

export interface Field {
  /** The variable the server reads, or a `${NAME}` in an OAuth app's address. */
  name: string;
  label: string;
  placeholder?: string;
  value?: string;
  secret?: boolean;
  help?: string;
}

/** Signs in with OAuth (a remote server). `${NAME}`s in `url` and `scope` come from `fields`. */
export interface OAuthConnect {
  kind: "oauth";
  url: string;
  scope?: string;
  /** Needs a client registered ahead with this provider; none means the server registers one itself. */
  provider?: Provider;
  fields?: Field[];
}

/** Set up with keys or nothing: the same lookup as /mcp add (the package pinned and checked), `${VAR}`s filled in. */
export interface KeysConnect {
  kind: "keys";
  /** An npm package, `pypi:<package>`, a URL or a GitHub repo. */
  source: string;
  fields: Field[];
  /** Header values, each referring to a field: `{ Authorization: "Bearer ${GITHUB_TOKEN}" }`. */
  headers?: Record<string, string>;
  setup?: { text: string; url: string };
}

export type Category = "Mail & calendar" | "Chat" | "Docs & files" | "Work tracking" | "Build";
export const CATEGORIES: Category[] = ["Mail & calendar", "Chat", "Docs & files", "Work tracking", "Build"];

export interface App {
  id: string;
  name: string;
  /** One line: what Marv can do with it. */
  blurb: string;
  /** A letter or two for its tile. */
  mark: string;
  category: Category;
  connect: OAuthConnect | KeysConnect;
  /** Another way in, offered under the main one ("Use an access token instead"). */
  alt?: { label: string; connect: KeysConnect };
  /** Said before connecting: preview status, admin consent… */
  note?: string;
}

export interface ProviderInfo {
  name: string;
  /** Where an admin registers the client. */
  console: { label: string; url: string };
  /** What to register, in one or two sentences. */
  steps: string;
  /** Fixed, so the redirect URI registered with the provider is known ahead. */
  callbackPort: number;
  /** Its token endpoint wants the client secret too. */
  secret: boolean;
}

export const PROVIDERS: Record<Provider, ProviderInfo> = {
  google: {
    name: "Google",
    console: { label: "Google Cloud › Credentials", url: "https://console.cloud.google.com/apis/credentials" },
    steps:
      "In a Google Cloud project, enable the Gmail, Calendar and Drive MCP APIs you want, set the OAuth consent screen to Internal (your Workspace only), then Create credentials › OAuth client ID › Desktop app.",
    callbackPort: 53682,
    secret: true,
  },
  microsoft: {
    name: "Microsoft",
    console: { label: "Entra admin center › App registrations", url: "https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade" },
    steps:
      "Register an app (accounts in this organization), add a Mobile and desktop redirect URI (below), add the Work IQ MCP server permissions you want, and grant admin consent.",
    callbackPort: 53683,
    secret: false,
  },
  slack: {
    name: "Slack",
    console: { label: "api.slack.com › Your apps", url: "https://api.slack.com/apps" },
    steps: "Create an app for your workspace (an internal app is enough), add the redirect URI below under OAuth & Permissions, and install it.",
    callbackPort: 53684,
    secret: true,
  },
  github: {
    name: "GitHub",
    console: { label: "GitHub › Developer settings › OAuth Apps", url: "https://github.com/settings/developers" },
    steps: "Create an OAuth App (your organization's, or your own) with the callback URL below.",
    callbackPort: 53685,
    secret: true,
  },
};

/** Marv's own registered clients: with one here, its provider's apps sign in with one click, nothing to set up. */
export const MARV_CLIENTS: Partial<Record<Provider, { clientId: string; clientSecret?: string }>> = {};

export const redirectUri = (provider: Provider) => `http://127.0.0.1:${PROVIDERS[provider].callbackPort}/callback`;

const GOOGLE_PREVIEW = "Google's MCP servers are in preview: your Google Cloud project may need to join the Workspace Developer Preview.";

export const APPS: App[] = [
  {
    id: "gmail",
    name: "Gmail",
    blurb: "Search and read mail, label it, draft replies for you to send.",
    mark: "M",
    category: "Mail & calendar",
    connect: {
      kind: "oauth",
      provider: "google",
      url: "https://gmailmcp.googleapis.com/mcp/v1",
      scope: "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/gmail.compose",
    },
    note: GOOGLE_PREVIEW,
  },
  {
    id: "google-calendar",
    name: "Google Calendar",
    blurb: "Your schedule, free time, and events to create or move.",
    mark: "31",
    category: "Mail & calendar",
    connect: {
      kind: "oauth",
      provider: "google",
      url: "https://calendarmcp.googleapis.com/mcp/v1",
      scope: "https://www.googleapis.com/auth/calendar.readonly https://www.googleapis.com/auth/calendar.events",
    },
    note: GOOGLE_PREVIEW,
  },
  {
    id: "outlook-mail",
    name: "Outlook",
    blurb: "Microsoft 365 mail: search, read, draft and reply.",
    mark: "O",
    category: "Mail & calendar",
    connect: {
      kind: "oauth",
      provider: "microsoft",
      url: "https://agent365.svc.cloud.microsoft/agents/tenants/${TENANT_ID}/servers/mcp_MailTools",
      scope: "https://agent365.svc.cloud.microsoft/agents/tenants/${TENANT_ID}/servers/mcp_MailTools/.default offline_access",
      fields: [
        {
          name: "TENANT_ID",
          label: "Tenant ID",
          placeholder: "00000000-0000-0000-0000-000000000000",
          help: "Entra admin center › Overview › Tenant ID.",
        },
      ],
    },
    note: "Uses Microsoft's Work IQ Mail server (preview); an admin grants consent once for your organization.",
  },
  {
    id: "slack",
    name: "Slack",
    blurb: "Search messages, read channels and threads, post and reply.",
    mark: "#",
    category: "Chat",
    connect: { kind: "oauth", provider: "slack", url: "https://mcp.slack.com/mcp" },
  },
  {
    id: "google-drive",
    name: "Google Drive",
    blurb: "Find and read Docs, Sheets and files; create new ones.",
    mark: "D",
    category: "Docs & files",
    connect: {
      kind: "oauth",
      provider: "google",
      url: "https://drivemcp.googleapis.com/mcp/v1",
      scope: "https://www.googleapis.com/auth/drive.readonly https://www.googleapis.com/auth/drive.file",
    },
    note: GOOGLE_PREVIEW,
  },
  {
    id: "notion",
    name: "Notion",
    blurb: "Search, read and update pages and databases.",
    mark: "N",
    category: "Docs & files",
    connect: { kind: "oauth", url: "https://mcp.notion.com/mcp" },
  },
  {
    id: "linear",
    name: "Linear",
    blurb: "Issues, projects and cycles: find, create, update.",
    mark: "L",
    category: "Work tracking",
    connect: { kind: "oauth", url: "https://mcp.linear.app/mcp" },
  },
  {
    id: "github",
    name: "GitHub",
    blurb: "Issues, pull requests, code search, CI runs.",
    mark: "GH",
    category: "Build",
    connect: { kind: "oauth", provider: "github", url: "https://api.githubcopilot.com/mcp/" },
    alt: {
      label: "Use an access token instead",
      connect: {
        kind: "keys",
        source: "https://api.githubcopilot.com/mcp/",
        headers: { Authorization: "Bearer ${GITHUB_TOKEN}" },
        setup: { text: "Create a fine-grained personal access token for the repositories Marv may use.", url: "https://github.com/settings/personal-access-tokens" },
        fields: [{ name: "GITHUB_TOKEN", label: "Access token", placeholder: "github_pat_…", secret: true }],
      },
    },
  },
  {
    id: "kicad",
    name: "KiCad",
    blurb: "Schematics, PCB layout, ERC and DRC, manufacturing exports.",
    mark: "Ki",
    category: "Build",
    connect: { kind: "keys", source: "pypi:mcp-server-kicad", fields: [] },
  },
];

/** `${NAME}` → its value. */
export function fillIn(text: string, values: Record<string, string>): string {
  return text.replace(/\$\{([A-Z0-9_]+)\}/g, (all, name: string) => values[name]?.trim() ?? all);
}

/** The catalog app a server of yours came from (by its name), if any. */
export const appFor = (name: string) => APPS.find((a) => a.id === name);

/** Your apps of `provider` holding a client of their own, for "use the same Google app as Gmail". */
export function clientsFor(provider: Provider, mine: { name: string; hasClient: boolean }[]): string[] {
  return mine.filter((m) => m.hasClient && appFor(m.name)?.connect.kind === "oauth" && (appFor(m.name)!.connect as OAuthConnect).provider === provider).map((m) => m.name);
}
