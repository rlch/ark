import { visit } from 'unist-util-visit';
import { execFileSync } from 'node:child_process';

/**
 * Remark plugin: renders ```d2 code blocks to inline SVGs at build time.
 * Requires `d2` CLI on PATH (https://d2lang.com/).
 * Ported from Modality — adapted for ark's Starlight config.
 */
export function remarkD2({ theme = 1, darkTheme = 200, pad = 20, sketch = true } = {}) {
	return (tree) => {
		visit(tree, 'code', (node, index, parent) => {
			if (node.lang !== 'd2' || !parent) return;

			const svg = renderD2(node.value, { theme, darkTheme, pad, sketch });

			parent.children[index] = {
				type: 'html',
				value: `<div class="d2-diagram">${svg}</div>`,
			};
		});
	};
}

function renderD2(source, { theme, darkTheme, pad, sketch }) {
	const args = [
		'--theme', String(theme),
		'--dark-theme', String(darkTheme),
		'--pad', String(pad),
		...(sketch ? ['--sketch'] : []),
		'-',
		'/dev/stdout',
	];

	let svg;
	try {
		svg = execFileSync('d2', args, {
			input: source,
			encoding: 'utf-8',
			maxBuffer: 10 * 1024 * 1024,
			stdio: ['pipe', 'pipe', 'pipe'],
		});
	} catch (err) {
		const msg = err.stderr?.toString() || err.message;
		console.error('[remark-d2] Failed to render diagram:\n', source.slice(0, 200), '\n', msg);
		const escaped = source.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
		return `<pre><code class="language-d2">${escaped}</code></pre><p style="color:red">D2 render error: ${msg.replace(/</g, '&lt;')}</p>`;
	}

	svg = svg.replace(/<\?xml[^?]*\?>\s*/, '');

	svg = svg.replace(
		/(<rect[^>]*)(stroke-width="0"\s*\/>)/,
		'$1style="fill:transparent" $2',
	);

	svg = svg.replace(
		/@media\s+screen\s+and\s+\(prefers-color-scheme:\s*dark\)\s*\{([\s\S]*?\.dark-code\s*\{[^}]*\})\s*\}/,
		(mediaBlock, body) => {
			const dataThemeRules = body.replace(
				/(\.)([a-zA-Z][\w-]*(?:\s+\.[a-zA-Z][\w-]*)*)\s*\{/g,
				':root[data-theme="dark"] .$2{',
			);
			return `${mediaBlock}${dataThemeRules}`;
		},
	);

	return svg;
}
