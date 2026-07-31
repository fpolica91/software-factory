# Repository Guidelines

## Project Structure & Module Organization

`docker-compose.yml` defines the factory infrastructure: Hatchet, PostgreSQL, Langfuse, MinIO, Qdrant, Redis, Ollama, and the worker services. TypeScript application code lives in `worker/`; entry points such as `worker.ts`, `plan.ts`, and `dispatch.ts` sit at its root, while Hatchet workflow definitions belong in `worker/workflows/`. Database bootstrap is handled by `postgres-init/01-databases.sh`. `sim-gate.sh` is a host-side integration harness for the external Boss simulation checkout, not a standalone test suite for this repository. Keep deployment guidance in `README.md` and configuration placeholders in `.env.example`.

## Build, Test, and Development Commands

- `cp .env.example .env && chmod 600 .env` creates a protected local configuration file; fill every required value before startup.
- `docker compose config` validates Compose syntax and resolved variables.
- `docker compose build factory-worker` rebuilds the TypeScript worker image.
- `docker compose up -d` starts or applies the complete stack; use `docker compose ps` and `docker compose logs -f factory-worker` to inspect it.
- `cd worker && npm ci` installs the locked Node dependencies.
- From `worker/`, `npm run worker` starts the worker, while `npm run dispatch -- ENG-431` and `npm run plan -- AISI-431` trigger individual flows. These require the relevant database, Hatchet, and API credentials.

## Coding Style & Naming Conventions

Follow the existing TypeScript style: two-space indentation, single quotes, semicolons, ESM imports, and explicit exported types. Use `camelCase` for functions and variables, `PascalCase` for types, and kebab-case workflow filenames (for example, `review-decision.ts`). Keep workflow tasks small and log meaningful transitions through Hatchet context. No formatter or linter is currently configured; match adjacent code and avoid unrelated reformatting.

## Testing Guidelines

There is currently no repository-local automated test command or coverage threshold. Before submitting, run `docker compose config`, rebuild the affected image, and exercise the changed entry point or HTTP workflow. Treat `sim-gate.sh` as environment-specific; document any subcommands used and their results in the pull request.

## Commit & Pull Request Guidelines

Recent history favors concise, imperative subjects prefixed with `factory:`, often ending with an issue reference: `factory: harden Linear retries (#27)`. Keep commits focused. Pull requests should explain behavior and configuration changes, link the issue, list verification commands and results, and include logs or dashboard screenshots when operational behavior changes.

## Security & Configuration

Never commit `.env`, tokens, generated credentials, session backups, or files under `secrets/`. Preserve the documented 64-hex-character requirement for `ENCRYPTION_KEY`, and update `.env.example` whenever a new environment variable becomes required.
