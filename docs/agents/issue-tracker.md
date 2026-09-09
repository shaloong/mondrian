# Issue tracker: Local Markdown

Issues and PRDs for this repository live temporarily as Markdown files under `.scratch/`.

## Conventions

- One feature per directory: `.scratch/<feature-slug>/`
- The PRD is `.scratch/<feature-slug>/PRD.md`
- Implementation issues are `.scratch/<feature-slug>/issues/<NN>-<slug>.md`, numbered from `01`
- Triage state is recorded as a `Status:` line near the top of each issue file
- Comments and conversation history append under a `## Comments` heading
- Delete the feature directory after all work is fixed, verified, and committed

## Publishing and fetching

When a skill says to publish to the issue tracker, create or update the appropriate file under `.scratch/<feature-slug>/`.

When a skill says to fetch a ticket, read the referenced local Markdown file.
