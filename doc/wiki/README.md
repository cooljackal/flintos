<!-- SPDX-License-Identifier: Apache-2.0 -->

# Wiki source (archived)

These pages were the source for the project's GitHub wiki. The reader-facing
documentation now lives in the Astro site under [`site/`](../../site), published
to [flintos.dev](https://flintos.dev). These files are kept as the migrated
source; edit the site for anything published.

Docs live in the repo so they can be reviewed in a PR alongside the change that
made them wrong, and so a checkout carries its own documentation.

## Publishing

Nothing here publishes automatically. An earlier `.github/workflows/wiki.yml`
synced this directory to the GitHub wiki on merge; it was retired when the docs
moved to the Astro site (`site/`, built and deployed by
`.github/workflows/site.yml`).

## Adding a page

Drop a `.md` file here, add it to `_Sidebar.md`, and link it from `Home.md` if
it belongs in one of the front-page tables. Filenames become URLs: hyphens, no
spaces.

## House style

Short. Plain language. Get to the point.

- Lead with the command.
- A table beats a paragraph.
- Include context only where its absence would leave the reader guessing.
- Say the trap out loud. "GPIO12 pulled high at boot can brick the module" is
  worth more than three paragraphs on strapping-pin theory.
- No page should need reading to the end before anything works.
