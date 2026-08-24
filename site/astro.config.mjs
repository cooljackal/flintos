// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

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
					],
				},
				{
					label: 'Developers',
					items: [
						{ label: 'Architecture', slug: 'developers/architecture' },
						{ label: 'Writing a driver', slug: 'developers/writing-a-driver' },
						{ label: 'Adding a board', slug: 'developers/adding-a-board' },
						{ label: 'Multicore', slug: 'developers/multicore' },
						{ label: 'API overview', slug: 'developers/api-overview' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ label: 'API docs (rustdoc)', link: '/api/', attrs: { target: '_blank' } },
					],
				},
			],
		}),
	],
});
