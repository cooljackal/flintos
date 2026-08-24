// @ts-check
import { existsSync, readFileSync } from 'node:fs';
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// The API reference is generated (`make apidoc`, issue #132): one collapsible
// group per crate, written to a git-ignored `_sidebar.json`. Read it if present
// so `astro build` works whether or not the reference has been generated yet --
// CI runs `make apidoc` first; a bare local dev checkout falls back to a stub.
const API_SIDEBAR = './src/content/docs/api/_sidebar.json';
const apiGroups = existsSync(API_SIDEBAR)
	? JSON.parse(readFileSync(API_SIDEBAR, 'utf8'))
	: [{ label: 'Not generated — run `make apidoc`', link: '/api/' }];

// https://astro.build/config
export default defineConfig({
	site: 'https://flintos.dev',
	integrations: [
		starlight({
			title: 'flintOS',
			// Swap the theme by changing the second entry: ember | slate | terminal | ocean
			customCss: [
				'@fontsource/ibm-plex-sans/400.css',
				'@fontsource/ibm-plex-sans/600.css',
				'@fontsource/ibm-plex-mono/400.css',
				'./src/styles/base.css',
				'./src/styles/themes/slate.css',
			],
			components: {
				SiteTitle: './src/components/SiteTitle.astro',
				Head: './src/components/Head.astro',
			},
			description: 'A dead-simple modern Rust RTOS for 32-bit microcontrollers.',
			social: [
				{ icon: 'github', label: 'GitHub', href: 'https://github.com/cooljackal/flintos' },
			],
			sidebar: [
				{
					label: 'Users',
					items: [
						{ label: 'Quickstart', slug: 'users/quickstart' },
						{ label: 'Hello, world', slug: 'users/hello-world' },
						{ label: 'Supported boards', slug: 'users/supported-boards' },
						{ label: 'Debug levels', slug: 'users/debug-levels' },
						{ label: 'Troubleshooting', slug: 'users/troubleshooting' },
						{ label: 'Upgrading', slug: 'users/upgrading' },
					],
				},
				{
					label: 'Developers',
					items: [
						{ label: 'Architecture', slug: 'developers/architecture' },
						{ label: 'Writing a driver', slug: 'developers/writing-a-driver' },
						{ label: 'Adding a board', slug: 'developers/adding-a-board' },
						{ label: 'Multicore', slug: 'developers/multicore' },
						{ label: 'Libraries', slug: 'developers/libraries' },
						{ label: 'API overview', slug: 'developers/api-overview' },
					],
				},
				{
					label: 'Hardware',
					collapsed: true,
					items: [
						{ label: 'Xtensa LX6', slug: 'hardware/arch-xtensa-lx6' },
						{ label: 'ESP32 (SoC)', slug: 'hardware/soc-esp32' },
						{ label: 'ESP32-DevKitC', slug: 'hardware/board-esp32-devkitc' },
						{ label: 'ESP32-WROVER', slug: 'hardware/board-esp32-wrover' },
						{ label: 'M5Stack Atom', slug: 'hardware/board-m5stack-atom' },
					],
				},
				{
					label: 'API Reference',
					collapsed: true,
					items: apiGroups,
				},
			],
		}),
	],
});
