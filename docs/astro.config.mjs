// @ts-check
import { defineConfig } from 'astro/config';
import mdx from '@astrojs/mdx';
import sitemap from '@astrojs/sitemap';
import remarkDirective from 'remark-directive';
import remarkAdmonitions from './scripts/remark-admonitions.mjs';
import remarkCodeTitles from './scripts/remark-code-titles.mjs';
import remarkLinkChecker from './scripts/remark-link-checker.mjs';
import { redirects } from './src/data/redirects.ts';

// Docs are served from <site>/docs/. `base` handles the prefix.
const BASE = '/docs';
// `remark-link-checker` both validates *and* rewrites relative links: under
// `build.format: 'directory'` (the default), source-relative `./foo` links
// resolve incorrectly in the browser (a page at `/programming_guide/x/`
// makes `./foo` mean `/programming_guide/x/foo`). The plugin emits absolute
// hrefs (`/docs/<slug>`) so links work regardless of trailing-slash quirks.
/** @type {any[]} */
const remarkPlugins = [
  remarkDirective,
  remarkAdmonitions,
  remarkCodeTitles,
  [remarkLinkChecker, { base: BASE }],
];

export default defineConfig({
  site: 'https://grepify.example',
  base: BASE,
  // `trailingSlash: 'always'` matches `build.format: 'directory'`: every
  // page lives at `<slug>/index.html` and is canonical at `<slug>/`.
  trailingSlash: 'always',
  integrations: [
    mdx({
      // MDX's own remark pipeline doesn't inherit `markdown.remarkPlugins`
      // reliably across Astro versions — wire admonitions + code titles
      // explicitly so .mdx content collection pages get them for sure.
      remarkPlugins,
    }),
    sitemap(),
  ],
  markdown: {
    remarkPlugins,
    shikiConfig: { theme: 'github-light', wrap: false },
  },
  redirects,
});
