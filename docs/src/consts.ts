// Site-wide constants. This is a self-hosted docs site for Grepify — a
// lightweight code index for any harness. SITE_URL is a placeholder until the
// fork has a public home; canonical URLs and sitemap entries derive from it.
export const SITE_URL = 'https://grepify.example';
export const GITHUB_REPO = 'https://github.com/grepify/grepify';

// `import.meta.env.BASE_URL` reflects `base` in astro.config.mjs (`/docs/`).
// DOCS_BASE and the URL builders below are the single source for the /docs base
// and the trailingSlash: 'always' convention — don't rebuild them per file.
export const DOCS_BASE = import.meta.env.BASE_URL.replace(/\/$/, '');
export const pageUrl = (slug: string) => new URL(`${DOCS_BASE}/${slug}/`, SITE_URL).toString();
export const pageMarkdownUrl = (slug: string) => new URL(`${DOCS_BASE}/${slug}.md`, SITE_URL).toString();
export const LLMS_TXT_URL = new URL(`${DOCS_BASE}/llms.txt`, SITE_URL).toString();
export const LLMS_FULL_TXT_URL = new URL(`${DOCS_BASE}/llms-full.txt`, SITE_URL).toString();
export const SKILL_MD_URL = new URL(`${DOCS_BASE}/skill.md`, SITE_URL).toString();
// GitHub web-editor URL prefix for the "Edit this page" link.
export const DOCS_EDIT_BASE = `${GITHUB_REPO}/edit/main/docs`;

// A content-collection id for `sources/index.md` is `sources/index`; the URL
// slug we want is just `sources`.
export const docSlug = (id: string) => id.replace(/\/index$/, '');

// Titles can mark 1–2 words with *asterisks* to italicize them in an accent
// color. titleText strips the markers for metadata; titleMarkup emits safe HTML.
const HTML_ESCAPES: Record<string, string> = {
  '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
};
const escapeHtml = (s: string) => s.replace(/[&<>"']/g, (c) => HTML_ESCAPES[c]);

export const titleText = (s: string): string => s.replace(/\*([^*]+)\*/g, '$1');

// Display title for a docs entry with the slug as fallback — guards both
// missing and empty-string titles so HTML pages and their .md twins agree.
export const docTitle = (id: string, title: unknown): string =>
  titleText(typeof title === 'string' && title.length > 0 ? title : docSlug(id));

export const titleMarkup = (s: string): string =>
  s.replace(/\*([^*]+)\*|([^*]+)/g, (_m, em, rest) =>
    em ? `<em>${escapeHtml(em)}</em>` : escapeHtml(rest),
  );

// Plain hex for <meta name="theme-color"> (CSS vars can't reach meta tags).
export const THEME_COLOR = '#faf7f2';
