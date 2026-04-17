// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import starlightLinksValidator from 'starlight-links-validator';
import starlightSidebarTopics from 'starlight-sidebar-topics';
import { remarkD2 } from './src/plugins/remark-d2.mjs';

export default defineConfig({
	markdown: {
		remarkPlugins: [remarkD2],
	},
	integrations: [
		starlight({
			title: 'ark',
			tagline: 'Zellij-native agent orchestration',
			plugins: [
				starlightLinksValidator({
					exclude: ['/learn/install/'],
				}),
				starlightSidebarTopics([
					{
						label: 'Learn',
						link: '/learn/install/',
						icon: 'open-book',
						items: [
							{
								label: 'Getting Started',
								items: [
									{ label: 'Install', slug: 'learn/install' },
									{ label: 'Quick Start', slug: 'learn/quick-start' },
									{ label: 'Roadmap', slug: 'roadmap' },
								],
							},
							{
								label: 'Tour',
								items: [
									{ label: 'Introduction', slug: 'learn/tour' },
									{ label: '1. Launch a Session', slug: 'learn/tour/01-launch' },
									{ label: '2. Author a Scene', slug: 'learn/tour/02-scene' },
									{ label: '3. Add a Reaction', slug: 'learn/tour/03-reaction' },
									{ label: '4. Wire an Extension', slug: 'learn/tour/04-extension' },
									{ label: '5. Hot-Reload', slug: 'learn/tour/05-reload' },
								],
							},
							{
								label: 'Concepts',
								items: [
									{ label: 'Agents', slug: 'learn/concepts/agent' },
									{ label: 'Scenes', slug: 'learn/concepts/scene' },
									{ label: 'Extensions', slug: 'learn/concepts/extension' },
									{ label: 'Mux (Zellij)', slug: 'learn/concepts/mux' },
									{ label: 'Supervisor', slug: 'learn/concepts/supervisor' },
								],
							},
						],
					},
					{
						label: 'Recipes',
						link: '/recipes/pin-agent-version/',
						icon: 'list-format',
						items: [
							{ label: 'Pin Agent Version', slug: 'recipes/pin-agent-version' },
							{ label: 'Add a Git Status Pane', slug: 'recipes/add-git-status-pane' },
							{ label: 'React to an Event', slug: 'recipes/react-to-event' },
							{ label: 'Custom Keybind', slug: 'recipes/custom-keybind' },
							{ label: 'Reload Without Respawn', slug: 'recipes/reload-without-respawn' },
							{ label: 'Author a Subprocess Extension', slug: 'recipes/author-subprocess-ext' },
						],
					},
					{
						label: 'Scenes',
						link: '/scenes/overview/',
						icon: 'puzzle',
						items: [
							{ label: 'Overview', slug: 'scenes/overview' },
							{ label: 'KDL Syntax', slug: 'scenes/kdl-syntax' },
							{ label: 'Scene Ops', slug: 'scenes/ops' },
							{ label: 'Reactions & Keybinds', slug: 'scenes/reactions-keybinds' },
							{ label: 'Plugin Lifecycle', slug: 'scenes/plugin-lifecycle' },
							{ label: 'Hot Reload', slug: 'scenes/hot-reload' },
							{ label: 'Schema Reference', slug: 'scenes/schema-reference' },
							{ label: 'CLI', slug: 'scenes/cli' },
						],
					},
					{
						label: 'Extensions',
						link: '/extensions/overview/',
						icon: 'information',
						items: [
							{ label: 'Overview', slug: 'extensions/overview' },
							{ label: 'Intent Protocol', slug: 'extensions/protocol' },
							{ label: 'ACP Extension', slug: 'extensions/acp' },
							{
								label: 'Authoring',
								items: [
									{ label: 'Compiled-In', slug: 'extensions/authoring/compiled-in' },
									{ label: 'Subprocess', slug: 'extensions/authoring/subprocess' },
									{ label: 'WASM Component', slug: 'extensions/authoring/wasm-component' },
								],
							},
							{ label: 'Capabilities', slug: 'extensions/capabilities' },
							{ label: 'CLI', slug: 'extensions/cli' },
						],
					},
					{
						label: 'Architecture',
						link: '/architecture/overview/',
						icon: 'laptop',
						items: [
							{ label: 'Overview', slug: 'architecture/overview' },
							{ label: 'Supervisor', slug: 'architecture/supervisor' },
							{ label: 'Mux', slug: 'architecture/mux' },
							{ label: 'Event Bus', slug: 'architecture/event-bus' },
							{ label: 'Hook IPC', slug: 'architecture/hook-ipc' },
							{ label: 'Testing', slug: 'architecture/testing' },
							{ label: 'Distribution', slug: 'architecture/distribution' },
						],
					},
					{
						label: 'Reference',
						link: '/reference/cli/',
						icon: 'document',
						items: [
							{ label: 'CLI', slug: 'reference/cli' },
							{ label: 'Configuration', slug: 'reference/config' },
						],
					},
				]),
			],
			social: [
				{ icon: 'github', label: 'GitHub', href: 'https://github.com/rlch/ark' },
			],
			head: [
				{
					tag: 'style',
					content: `
.d2-diagram { margin: 1.5rem 0; }
.d2-diagram svg { max-width: 100%; height: auto; }
`,
				},
			],
			customCss: ['./src/styles/custom.css'],
		}),
	],
});
