/** The computer as the web view sees it (the user agent is all there is without a Tauri plugin). */
const ua = navigator.userAgent;
export const MAC = /Mac OS X|Macintosh/.test(ua);
export const WINDOWS = ua.includes("Windows");
export const LINUX = /Linux/.test(ua) && !/Android/.test(ua);
