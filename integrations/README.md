# Factory Integration Adapters

This package is the vendor-neutral boundary between the durable Factory
lifecycle and external work trackers, source hosts, CI systems, and artifact
stores. Codex core and `factory-runtime` never import it.

An adapter registers one or more capabilities with `IntegrationRegistry`:

```ts
registry.register({ intake: tracker, sourceHost, artifacts });
```

Distributable adapters export a `FactoryIntegrationPlugin`; factoryd-side
workers call `installIntegration(registry, plugin, config)` during startup.
Configuration stays with the deployment, while adapter names are persisted in
job input so recovery selects the same implementation.

`loadIntegrations` accepts an explicit list of ESM module specifiers and JSON
configuration. There is no built-in tracker or source host, and loading a
plugin never changes the Codex runtime.

The workflow worker reads that list from `FACTORY_INTEGRATION_PLUGINS_JSON`:

```json
[{"module":"file:///opt/factory-plugins/tracker/index.js","config":{}}]
```

A durable job selects the intake adapter without embedding vendor logic:

```json
{"integration":{"intake":{"adapter":"tracker","externalId":"WORK-42"}}}
```

`IntakeAdapter` preserves work intake, operator messages, decomposition, and
lifecycle updates. `SourceHostAdapter` preserves change requests, review
threads, and CI evidence. `ArtifactAdapter` stores large outputs outside job
state. Hatchet/factoryd workers call these adapters and persist progress before
or after external effects so retries remain part of the durable lifecycle.
Every lifecycle event carries a deterministic `eventId`; adapters must use it
as their idempotency key. Publication occurs before factoryd commits the
attempt. If delivery fails after Codex completed, the completed checkpoint is
recovered and delivery is retried without running the model stage again.

No vendor is built into the contract. Linear/GitLab implementations may be
added later as plugins, just like GitHub or another system. MinIO is an
optional `ArtifactAdapter`; enabling its Compose profile does not change the
Factory protocol or Codex runtime.
