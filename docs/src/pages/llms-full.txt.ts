// Generates /docs/llms-full.txt — the full text of every docs page
// concatenated into one file, for LLMs and agents that want the
// programming guide + connector reference in a single fetch
// (see https://llmstxt.org/). /docs/llms.txt is the lighter index.
import type { APIRoute } from 'astro';
import { getCollection } from 'astro:content';
import { docSlug, docTitle, pageUrl } from '../consts';
import { flatten } from '../data/docs-sidebar';
import { mdxToMarkdown, docSrcDir } from '../lib/raw-markdown';

// Include-fragments are embedded verbatim in their host page (cli.mdx inlines
// cli_commands.mdx), so emitting them standalone would duplicate the content.
const INCLUDE_FRAGMENTS = new Set(['cli_commands']);

export const GET: APIRoute = async () => {
  const docs = await getCollection('docs');
  const bySlug = new Map(docs.map((d) => [docSlug(d.id), d]));

  const out: string[] = [
    '# Grepify Docs — full text',
    '',
    '> The complete Grepify documentation concatenated ' +
      'into one file for LLMs and agents. Each section below is one docs page, ' +
      'in reading order. ' +
      'For a lighter index of pages, see /docs/llms.txt.',
  ];

  const seen = new Set<string>();
  const emit = (slug: string) => {
    const d = bySlug.get(slug);
    if (!d || seen.has(slug)) return;
    seen.add(slug);
    const title = docTitle(d.id, d.data.title);
    out.push('', '---', '', `# ${title}`, '', `Source: ${pageUrl(slug)}`, '', mdxToMarkdown(d.body, docSrcDir(d)));
  };

  // Sidebar order first, then any stray docs not in the tree.
  for (const e of flatten()) emit(e.slug);
  for (const slug of bySlug.keys()) if (!INCLUDE_FRAGMENTS.has(slug)) emit(slug);

  return new Response(out.join('\n') + '\n', {
    headers: { 'Content-Type': 'text/plain; charset=utf-8' },
  });
};
