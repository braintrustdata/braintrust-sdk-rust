# Ruby and Rust SDK API alignment reference

## Scope and alignment requirements

Ruby is a structure and familiarity reference for the Rust SDK’s existing public API. This companion to the [refactor plan](refactor-plan.md) owns the concept mappings, source references, capability inventory, parity backlog and open alignment decisions. The plan owns the target architecture, responsibilities, completion contracts and staged implementation roadmap.

Use similar entry points, selectors, operation names and result concepts where useful, with idiomatic Rust ownership, errors and sync/async behavior. Strict alignment and feature-for-feature parity are not requirements. The current refactor improves what already exists; capability gaps below document future work without gating implementation or release.

## Reference implementations

Inspected Rust baseline: `f2216fa`. Ruby reference: `~/Code/sdk-ruby` at `56ff070cd0a21713f0b1a163cd2573d8858a2b22`. These observations describe those checkouts, not a claim of full cross-SDK feature parity.

| Ruby reference | Public concept or boundary | Proposed Rust application |
| --- | --- | --- |
| `lib/braintrust.rb`, `config.rb`, `state.rb` | Initialization, configuration and explicit or default state | Explicit client/session construction with matching settings and precedence; global convenience remains optional |
| `lib/braintrust/api.rb`, `api/datasets.rb` | Namespaced endpoint access | Resource-specific API clients behind high-level handles |
| `lib/braintrust/api/functions.rb` | Function list, create, get, invoke and delete | Corresponding typed API namespace when function capabilities are delivered |
| `lib/braintrust/trace.rb`, `trace/span_processor.rb` | Tracing, parent/destination context, filtering, flush and permalinks | Primary tracing surface; Rust span handles and lifecycle adapters preserve the same user concepts |
| `lib/braintrust/trace/span_origin.rb`, `internal/origin.rb` | Trace provenance and dataset-record origin | Shared provenance values and wire conversion, separate from destination and ancestry |
| `lib/braintrust/dataset.rb` | Dataset reference and lazy, versioned enumeration separate from endpoint requests | Concrete dataset handle plus query/pagination objects |
| `lib/braintrust/prompt.rb` | Load a versioned prompt and build provider parameters | Prompt retrieval separate from local rendering and provider invocation |
| `lib/braintrust/functions.rb`, `task.rb`, `scorer.rb`, `classifier.rb` | Reusable callables and remote function adapters | Task/scorer/classifier concepts shared beyond evaluation; explicit Rust callable adapters |
| `lib/braintrust/contrib.rb`, `contrib/*` | Integration registry, automatic or explicit instrumentation | Provider integration boundaries with explicit Rust wrappers/extensions where patching is inappropriate |
| `lib/braintrust/trace/attachment.rb` | Attachment construction and provider-compatible representation | Attachment capability owned by tracing/content, with file/network work separate from values |
| `lib/braintrust/server.rb`, `server/rack.rb`, `server/services/*` | Evaluator listing and remote execution | Explicit parity backlog for server-facing capabilities; do not confuse this with the existing Rust `bt` CLI protocol |
| `lib/braintrust/eval/evaluator.rb` | Reusable task/scorer definition delegates execution | `Evaluator<I, O>` independent of an individual run |
| `lib/braintrust/eval/context.rb` | Normalize inputs before execution | Validated `EvalPlan<I, O>` assembled once |
| `lib/braintrust/eval/runner.rb` | Per-case state and execution orchestration | Focused executor, case runner, and result accumulator |
| `lib/braintrust/eval/result.rb`, `summary.rb`, `formatter.rb` | Result, summary, and presentation separated | Structured results with separate formatting/reporters |
| `lib/braintrust/internal/retry.rb` | Retry mechanism outside an individual client | Shared internal retry executor with operation-specific classification |
| `lib/braintrust/trace/span_processor.rb` | Processor decorates and delegates to another processor | Explicit composition rather than one component owning every concern |

Ruby’s public model informs all of these areas. Do not copy dynamic argument normalization, monkey-patching or process-global ownership into Rust merely to match spelling. Conversely, different internals do not justify unrelated user workflows. Ruby uses OpenTelemetry/OTLP for tracing; the Rust implementation currently uses direct ingestion. Reusing the Rust transport is an internal choice, not proof that every current raw-ingestion helper should remain public or that tracing concepts may diverge.

Missing capabilities are future-work notes, outside the current refactor. Existing useful Rust capabilities can remain even where Ruby has no identical helper. Improve their placement and naming without enforcing parity or removing behavior merely to match a matrix.

### Capability alignment across the entire API

This is an initial source-backed map. “Gap” means the inspected Ruby public API does not establish a direct equivalent; shared backend support alone is insufficient. Use these observations during API design; unmatched rows are not implementation gates.

| Area | Ruby shape | Current Rust / target disposition |
| --- | --- | --- |
| Setup and configuration | `Braintrust.init`, `Config`, `State`, explicit state injection | Align client/session settings, explicit credentials and default project semantics; Rust runtime ownership stays explicit |
| API namespaces | `API.new(state: ...)`, `.datasets`, `.functions`, `.login`, object permalinks | Expose an `api()` view over shared services; do not require a new project-handle hierarchy for ordinary operations |
| Manual tracing | OpenTelemetry tracer, `in_span`, explicit attributes and parent context | Native Rust tracer/span and scoped callback equivalents; preserve input/output/expected/error/metadata/tags/scores concepts |
| Trace routing and provenance | Project/experiment parent attributes, span origin, environment | Align destination/default/precedence semantics; distinguish ancestry from routing and keep SDK-specific identity values |
| Trace lifecycle and links | Processor flush/shutdown and `Trace.permalink` | Matching concepts with Rust-specific queue acceptance, delivery errors, barriers and ownership |
| Dataset reads | `Dataset.new(name:, project:, version:)` or ID; lazy enumeration and `fetch_all` | Align name/ID/project/version selectors, lazy fallible iteration and eager convenience; retain record origin |
| Dataset API operations | `API::Datasets.list/get/get_by_id/create/insert/delete/fetch/permalink` | Use comparable namespaces and request/result concepts; distinguish deleting a dataset from deleting a record |
| Dataset record patches/deletes and summaries | No dedicated public record-update/delete or dataset-summary helper identified in inspected Ruby files | Gap: verify whether generic event insertion provides the same operation; retain existing behavior and document the difference for future alignment work |
| Experiment setup and eval results | Internal project/experiment creation used by `Eval.run`; experiment identity, permalink, statistics and errors in `Eval::Result` | Keep experiment creation/identity services; a mandatory public `Project`/`Experiment` handle model is not established by Ruby |
| Experiment logging, feedback, remote comparison | No direct public `Experiment.log`, feedback or remote-comparison facade identified | Gap for current Rust helpers: distinguish normal trace recording and local eval summary from genuinely additional product operations; preserve current capabilities while improving their placement and naming |
| Tasks, scorers and evaluation | Top-level `Task`/`Scorer`, reusable `Eval::Evaluator`, `Eval.run`, structured results | Align callable names, parameters, case sources, score metadata and results; Matching function/closure implementations of `Task` and `(name, closure)` implementations of `Scorer` replace Ruby mixins/keywords; explicit adapters handle blocking callbacks |
| Prompt retrieval and rendering | `Prompt.load(project/project_id, slug, version, defaults)`, `.build` | Missing Rust capability; reserve matching boundaries and plan as an explicit parity addition |
| Remote functions and classifiers | `API::Functions`, `Functions.task/scorer`, top-level `Classifier` | Missing Rust capabilities; shared callable concepts should accommodate them without redesigning the eval engine |
| Provider instrumentation and usage | `Contrib` instrumentation; provider support normalizes metrics | Rust usage extraction is a lower-level helper for the same capability; align metric meanings and wrappers rather than exposing provider schemas as the core API |
| Streaming | Provider-specific instrumentation accumulates streaming responses | Same provider-observation capability; detailed semantics and supported providers require fixtures in the separate streaming phase |
| Attachments | `Trace::Attachment.from_bytes/from_file/from_url`, provider-compatible values | Missing Rust first-class capability; track separately from arbitrary JSON passthrough |
| Propagation and deterministic replay | OpenTelemetry trace context; no identical Ruby `SpanComponents`/row-replay helper identified | Preserve the existing codec/deterministic ID capabilities; improve abstraction boundaries without requiring a matching Ruby helper |
| Raw logs3 upload and queue switches | Ruby's tracing export uses OTLP and an internal processor | Expose a plain API client for direct manual ingestion; keep retries/queueing in the SDK and test scheduling private. Replace checked delivery with async full-delivery awaits |
| Repository metadata settings | No corresponding public collection API established by this review | Assess unused Rust configuration exports on their own utility; missing Ruby equivalence is not a removal criterion |
| CLI and remote evaluation server | Ruby setup CLI and Rack evaluation services; Rust `BtEvalRunner` uses a different protocol | Tooling adapters, not one claimed shared API; track each supported workflow explicitly |

Acceptance of alignment means a developer familiar with Ruby recognizes the workflow and capability in Rust. It does not mean claiming an unimplemented Ruby-equivalent method, preserving an accidental Rust export, or copying Ruby exception suppression. Meaningful Rust errors and the sync/async distinction remain requirements.

## Public API shape and semantics

The public entry points should resemble Ruby's `init`/explicit state, `API`, `Trace`, `Dataset`, `Task`, `Scorer` and `Eval` rather than require `Client → Project → Logger` for everything. A Rust `Client` is the explicit owner of session/services; `client.api()` and `client.tracer()` are views over that owner. A project selector scopes an operation without necessarily requiring a separate network-fetched handle. Existing project/experiment conveniences can delegate to these entry points without requiring exact Ruby equivalence. The crate root exposes one resource API whose ordinary methods immediately attempt submission and directly return `Completion<T, E>`. Work progresses without awaiting; awaiting only retrieves its value/error. The token stores issuance `Result<(), E>` and exposes it through immutable `Deref` for optional synchronous inspection. Issuance means response setup/scheduling succeeded; only ingestion writes enter the logs queue/batcher. Reads and API requests run as independent response tasks. Sender and receiver are constructed together, with the receiver always stored in the token and its sender handed to the response producer. Awaiting returns that issuance error or the operation's normal result as `Result<T, E>`, allowing `span.log(input).await?` without a separate issuance-result step. Tokens have no blocking or scheduling methods. There is no separate operation type or parallel sync/async resource hierarchy. Each client selects one runtime through `Option<tokio::runtime::Handle>`: `Some(handle)` explicitly selects that runtime; `None` uses the internal runtime. Both call styles, the queue, background worker and I/O share that binding. There is no automatic runtime detection; `current_runtime()` is an explicit convenience returning the optional current handle. Callers overriding the default own that runtime's lifetime and progress requirements, including when tokens are never awaited.

API collections retain Ruby's operation vocabulary and behavior: dataset `list`, lookup by project/name or ID, idempotent registration, insertion, dataset deletion, record fetching and permalinks. Preserve idempotent create behavior where Ruby documents it rather than imposing a universal “create always means new” rule. Eval experiment creation separately distinguishes a new experiment from reuse. Typed Rust selectors and a single input struct with `Default` for optional fields replace keyword combinations without a dynamic normalization subsystem; required fields remain explicit. Collection accessors are lightweight views rather than separately owned clients.

## Future capability work

The following are future-work notes, not part of the current refactor or release criteria: versioned prompt loading/rendering, broader function APIs, remote task/scorer adapters, classifiers, attachments, provider instrumentation/filtering expansion and evaluator server workflows. Evaluate priorities separately when those capabilities are requested.

Differences in current experiment/feedback/comparison helpers, record patches/deletes, summaries, manual ingestion, propagation and repository metadata do not block this plan. Preserve useful existing behavior, use more familiar API placement where practical, and document remaining differences here. Do not require each feature to acquire a Ruby counterpart before improving it.

The refactor plan owns the concrete layering: one public API with eager submission and completion tokens over shared async services, one runtime-bound execution context per client and a plain one-attempt HTTP API client. Stored issuance results and awaited completion results distinguish acceptance from actual results for every scheduled capability, including direct API calls, reads, evals and lifecycle work. This reference supplies context without imposing a separate feature-parity workstream.
