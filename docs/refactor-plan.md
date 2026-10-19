# Rust SDK architecture and public release refactor plan

## Status and agreed constraints

**Status:** Implementation authorized October 20, 2026; deliver the roadmap as incremental PRs. Release publication remains a separate action. Type names and signatures below are proposals, not existing APIs. Updated October 20, 2026.

The goal is to improve the existing Rust SDK around a familiar resource model, using Ruby’s structure as a reference with idiomatic Rust ownership, types, errors and sync/async calling conventions. Small internal components provide clear ownership, explicit completion guarantees, and one implementation of each behavior. We can break the unreleased API, but should preserve backend compatibility and account for existing git-pinned consumers during the transition.

Agreed requirements:

- Use Ruby to improve the shape and familiarity of existing capabilities. Exact alignment and feature-for-feature parity are not requirements. Document gaps in the [API alignment reference](api-alignment.md) as future work outside this refactor.
- Every supported capability has a synchronous interface. Async support complements it; users of the synchronous API never need to manage Tokio or futures.
- One public resource API synchronously attempts submission and directly returns `Completion<T, E>`. The token stores `Result<(), E>` for immutable synchronous inspection through `Deref`; awaiting returns either that admission error or the completed operation result. Work is scheduled when the method is called, whether or not its token is awaited. Awaiting only retrieves the result. There is no separate public operation type or blocking/scheduling method on the token, and no parallel sync/async resource hierarchy.
- Each client has one runtime binding for both async and sync execution: `Some(handle)` selects an explicitly supplied runtime; `None` selects the SDK-owned internal runtime. There is no automatic current-runtime detection. `current_runtime()` explicitly captures the current Tokio handle for that option. The background worker and all operation I/O use the same selected runtime.
- Completion is an observation mechanism, not an ingestion queue. SDK log writes enter the bounded delivery queue and complete after delivery; reads and direct HTTP calls run as tasks on the bound runtime and complete with their normal responses/errors. Evals and lifecycle work have their own orchestrators. The token's stored issuance result reports immediate setup/scheduling failure; it does not imply logs-batcher admission for every call.
- Delivery runs automatically. There is no public automatic/manual delivery mode or test-driven `checked_delivery` switch in the target API. Explicit flush and shutdown are lifecycle operations, not delivery modes.
- Complex types orchestrate focused collaborators. In particular, an uploader does not own a private retry engine or batching algorithm.
- Resource identity, authentication scope, trace ancestry, event content, and delivery state are separate concepts.
- Evaluation execution, recording, reporting, and CLI integration have small internal owners.
- Streaming decomposition is a separate phase. Earlier phases preserve its behavior through an adapter rather than redesigning it incidentally.
- Delivery happens through incremental, problem-focused PRs, potentially stacked. Each merged layer must be usable and tested without unfinished upper layers.

The shared async execution pipeline is the behavioral source of truth and main test interface. Public methods submit commands through one execution binding; completion tokens observe results without duplicating resource implementations. The plain HTTP API client supports direct ingestion and independent test validation. Fake dependencies and processor controls are test-only. Failure-reporting choices are explained below.

The Rust snippets are proposed interface sketches. Imports, supporting value definitions and implementation bodies are omitted. Semicolon-terminated declarations inside `impl` blocks are documentation notation, not compilable Rust. They make ownership, inputs, outputs and call relationships reviewable without implementing the refactor.

## Current problems

Rust baseline: `f2216fa`. The [API alignment reference](api-alignment.md) contains the Ruby/Rust concept mappings, capability gaps, source references and parity backlog. Ruby informs the existing API’s structure; missing Ruby features do not gate this roadmap.

The Rust changes should address these concrete starting points:

- `src/logger.rs` combines configuration, login, resource endpoints, span updates, queue integration, and runtime-sensitive destruction.
- Public handles expose private backend trait bounds; the root namespace exports implementation and provider details together.
- `src/logs3.rs` and `src/log_queue/http.rs` duplicate capability discovery, normal and overflow uploads, and retry behavior. Their retry schedules and attempt counts differ. The standalone uploader also handles serialization, splitting after 413, and result accounting.
- `Experiment::ensure_registered` caches invented metadata on registration failure. Experiment clones reset registration state. Dataset initialization caches failure strings, and dataset pagination converts request errors into end-of-data.
- Dataset `flush` only submits its private pending rows, whereas span `flush` waits on the shared queue. Experiment pending-span tracking adds another lifecycle mechanism.
- Explicit experiment credentials mutate shared login state, unlike the span credential path.
- Evaluation ignores project/experiment options, does not use its root as the case parent, hides some failures, and requires a client even for CLI local mode.
- README construction examples are stale; the synchronous evaluation example uses two runtimes. Several fallible builders never fail, while some network-dependent APIs cannot report their real failures.

Source anchors: [public exports](../src/lib.rs), [client](../src/logger.rs), [standalone upload](../src/logs3.rs), [queue upload](../src/log_queue/http.rs), [dataset](../src/dataset.rs), [experiment](../src/experiments/experiment.rs), [evaluation](../src/eval/runner.rs), [CLI adapter](../src/eval/bt_runner.rs), [streaming](../src/stream.rs).

## Target architecture and public entry points

```text
public resource or API method
        |
create (sender, Completion) pair; attempt scheduling now
        +------------------------------> return Completion<T, E>
        |                                  optional .await
        v
client's one selected runtime
        |
        +-- SDK ingestion -> bounded log queue -> delivery processor
        |                    -> batcher/retries/upload -> tracker sends receipt
        |
        +-- read / direct API -> resource/endpoint task -> sender sends response
        |
        +-- eval / lifecycle -> dedicated orchestration -> sender sends report
```

Use one set of public resource types and input/result values. `span.log(input)` immediately attempts submission and returns its token; `span.log(input).await?` propagates either admission or completion failure. Synchronous callers can borrow the stored issuance result when they need to react immediately. There is no separate public `sync`, `async` or `futures` resource hierarchy. Awaiting a previously returned token does not move the accepted work to the observing runtime.

Dependency rules:

1. Public methods create a sender/completion pair and schedule the relevant operation synchronously. Ingestion additionally requires bounded log-queue admission. Other operations do not enter that queue or the batcher. Network work starts on the bound runtime without token polling; no network I/O runs inline on the caller.
2. Scheduling uses the client's execution context; tokens only observe completion. Each operation's actual executor owns its sender and completes it with the operation's normal result. Small async services own resource behavior, delivery and evaluation; there is no duplicate synchronous implementation or third facade solely for tests.
3. The plain API endpoint layer encodes requests, sends one request, and decodes responses/errors. It has no SDK retry, batching, background login or resource-resolution policy. Its execution adapter schedules direct requests without routing them through the SDK log processor.
4. Login is a direct API request. Organization selection, deferred login and retries belong to SDK session handling. Explicit credentials construct a direct client without priming shared mutable login state.
5. Clones share logical identity and immutable credential scopes. Runtime-bound queues, services and transport pools belong to execution contexts; they do not migrate between runtimes.
6. Generic mechanisms accept protocol-specific policy without knowing span lifecycle, resource names or CLI output.
7. Keep one crate initially. Public exports describe capabilities; implementation details stay private.

## Plain HTTP API client

Use [PR #63](https://github.com/braintrustdata/braintrust-sdk-rust/pull/63) only as a source for the request/response types and endpoint interfaces needed by existing SDK operations, direct ingestion and validation. Do not merge or rebase onto its branch, import whole endpoint families, or inherit its session machinery. Implement the small one-attempt HTTP layer around that selected contract on the current base.

```rust
// api — independently usable direct HTTP API; one public operation surface
pub struct Client { execution: ExecutionBinding, config: ApiConfig }
impl Client {
    pub fn new(config: ApiConfig) -> Result<Self, InitError>;
    pub fn login(&self, request: LoginRequest)
        -> Completion<LoginResponse, ApiError>;
    pub fn datasets(&self) -> Datasets;
    pub fn experiments(&self) -> Experiments;
    pub fn logs3(&self) -> Logs3;
    pub fn btql(&self) -> Btql;
}
impl Logs3 {
    pub fn insert(&self, request: InsertRows)
        -> Completion<InsertResponse, ApiError>;
    pub fn request_overflow(&self, request: OverflowInput)
        -> Completion<OverflowGrant, ApiError>;
}
impl Datasets {
    pub fn fetch(&self, query: RecordQuery)
        -> Completion<RecordPage, ApiError>;
    pub fn insert(&self, id: DatasetId, rows: Vec<RecordInput>)
        -> Completion<InsertResult, ApiError>;
}

// Private endpoint implementation, called on its owning execution context.
impl Logs3Endpoint {
    async fn insert_once(&self, request: InsertRows) -> Result<InsertResponse, ApiError>;
}
```

Construction validates configuration; runtime-bound transport is initialized on the selected execution context, and it does not send a login request. Executing explicit login makes one request; completion contains its response. The SDK decides whether/how to retry and derives an immutable scope with the organization-specific endpoint. Direct API configuration accepts explicit credentials/endpoints without requiring a mutable SDK `LoginState`.

Direct ingestion does not split, merge, requeue, retry or silently switch to overflow. Awaiting it returns its one-attempt result; successful synchronous admission returns a token whose eventual result contains the same HTTP error or response. Scheduling direct HTTP work is separate from SDK ingestion batching. An oversized/rejected request retains its actual error. Callers can issue explicit protocol requests themselves, or use SDK ingestion for reliability. Use this public client for independent test setup/readback, too. Backend acknowledgment is separate from query visibility: eventual-consistency polling belongs in a bounded validation helper, not hidden inside a one-attempt API method.

### Transport dependencies

```rust
trait HttpTransport: Send + Sync {
    fn execute(&self, request: HttpRequest)
        -> BoxFuture<'_, Result<HttpResponse, TransportError>>;
}
trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep_until(&self, at: Instant) -> BoxFuture<'_, ()>;
}
```

The factory configures TLS roots, proxies, timeouts and User-Agent consistently. Private constructors accept fake transport/time in tests. Preserve operation/status/source context in errors. Signed uploads must not inherit Braintrust bearer credentials.

## Eager submission, completion tokens and runtime ownership

```rust
// Internal transport for a single response, independent of the log queue.
type CompletionReceiver<T, E> = tokio::sync::oneshot::Receiver<Result<T, E>>;
type CompletionSender<T, E> = tokio::sync::oneshot::Sender<Result<T, E>>;
pub struct Completion<T, E> {
    issued: Result<(), E>, // setup/scheduling result; Ok does not mean delivered
    receiver: CompletionReceiver<T, E>, // created immediately, always present
}
impl<T, E> Completion<T, E> {
    // Private construction; no caller sees a token until issuance is recorded.
    pub(crate) fn pair() -> (CompletionSender<T, E>, Self);
    pub(crate) fn record_issuance(&mut self, result: Result<(), E>);
}
impl<T, E> Deref for Completion<T, E> {
    type Target = Result<(), E>;
    fn deref(&self) -> &Self::Target; // borrows self.issued; never waits
}
impl<T, E> IntoFuture for Completion<T, E> {
    type Output = Result<T, E>;
    type IntoFuture = CompletionFuture<T, E>;
    fn into_future(self) -> Self::IntoFuture;
}

// One operation-specific E describes admission and post-admission failures.
pub enum WriteError {
    Admission(EnqueueError),
    Execution(ExecutionError), // worker/runtime loss after acceptance
    Delivery(DeliveryError),
}
impl From<EnqueueError> for WriteError {
    fn from(error: EnqueueError) -> Self;
}

pub struct Client { state: Arc<ClientState> }
struct ClientState {
    config: ClientConfig,
    resources: Arc<ResourceRegistry>, // logical identity; no runtime-owned I/O
    execution: ExecutionBinding,
}
pub struct Span { state: Arc<Mutex<SpanState>>, execution: ExecutionBinding }
pub struct Dataset { state: Arc<DatasetState>, execution: ExecutionBinding }

impl Client {
    pub fn new(config: ClientConfig) -> Result<Self, InitError>;
    pub fn api(&self) -> api::Client;
    pub fn tracer(&self, target: Destination) -> Tracer;
    pub fn dataset(&self, selector: DatasetSelector)
        -> Completion<Dataset, ApiError>;
    pub fn flush(&self)
        -> Completion<FlushReport, FlushError>;
    pub fn shutdown(&self, timeout: Duration)
        -> Completion<ShutdownReport, ShutdownError>;
}
impl Span {
    pub fn log(&self, input: LogInput)
        -> Completion<DeliveryReceipt, WriteError>;
    pub fn end(&self)
        -> Completion<DeliveryReceipt, WriteError>;
    pub fn context(&self) -> SpanContext;
}
impl Dataset {
    pub fn fetch_page(&self, query: RecordQuery)
        -> Completion<RecordPage, ReadError>;
    pub fn fetch_all(&self, query: RecordQuery)
        -> Completion<Vec<Record>, ReadError>;
}

#[derive(Default)]
pub struct LogInput {
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub metadata: Map<String, Value>,
    // Other existing optional event fields have one typed home.
}
```

`CompletionReceiver` is only an internal alias for a one-shot response receiver, not a future-created receiver or another delivery subsystem. Constructing the pair immediately creates both channel endpoints, stores the receiver inside `Completion` and returns the sender to the code that will produce the result. A pending receiver already exists; only its value arrives later. Tokio's [one-shot channel](https://docs.rs/tokio/latest/tokio/sync/oneshot/fn.channel.html) supplies this mechanism.

The stored `issued: Result<(), E>` means token issuance/setup and scheduling succeeded, not that a call necessarily entered a particular queue. For a log write, issuance includes bounded queue admission. For a read or direct API call, it includes arranging the response-producing task on the selected runtime. The async result is that method's normal response/error, not a generic ingestion receipt. Evals complete with run results; lifecycle calls complete with reports. Only operations which actually produce ingestion rows use the log queue and batcher.

```rust
// Internal sequence for a log write; all issuance recording precedes return.
let (sender, mut completion) = Completion::pair();
let issued = writes.enqueue(command, sender).map_err(WriteError::from);
completion.record_issuance(issued);
completion

// A read instead hands its sender to a directly scheduled task, not the batcher.
let (sender, mut completion) = Completion::pair();
let issued = execution.spawn_response(sender, move || async move {
    dataset_service.fetch_page(&scope, query).await
}).map_err(ReadError::from);
completion.record_issuance(issued);
completion
```

The pair and issuance-recording methods stay private. The constructor initializes the receiver immediately; the scheduling attempt records a final issuance result before the token escapes the public method. If scheduling/enqueueing fails, retain that error in the token and release the unused sender. The receiver still exists, but awaiting returns the stored error before polling it. This preserves the original `E` without cloning it or sending a duplicate copy through the channel. No externally visible pending issuance state or mutable result is introduced.

On success, the task or delivery outcome tracker owns the sender and calls `send(Result<T, E>)` once. A failed send means the observer has dropped; the producer handles that outcome directly without a wrapper. `IntoFuture` consumes the token, checks issuance and then awaits the already-existing receiver. Dropping the producer unexpectedly maps channel closure to an execution/worker-loss error; it is not EOF or success. Dropping the observer does not cancel the producer, and an undeliverable response must not undo completed work. Producer errors still reach the appropriate diagnostics and trackers. Repeated end/lifecycle calls which share execution each get their own pair; the shared outcome state fans out to their senders rather than trying to clone a one-shot receiver.

Immutable `Deref<Target = Result<(), E>>` supports optional synchronous inspection through borrowed `Result` methods or `&*completion`. `Ok(())` records only issuance/setup, so it stays `Ok` if an accepted task later fails. Awaiting the same token still reports a stored issuance failure or the actual operation result as `Result<T, E>`. Error variants preserve immediate scheduling/admission versus later execution/operation failures, without an outer result or `E: Clone` bound.

```rust
// Calling the method attempts issuance/scheduling immediately.
let completion = span.log(LogInput { output: Some(value), ..Default::default() });

// Optional synchronous inspection borrows the issuance result.
if let Err(error) = &*completion {
    // React to immediate setup/scheduling failure.
}

// Observes that issuance error, or the operation's eventual result.
let receipt = completion.await?;

// Async callers do not need a separate admission-result step.
let receipt = span.log(LogInput::default()).await?;

// Reads use the same token shape.
let pending_dataset = client.dataset(selector);
let dataset = pending_dataset.await?;
```

The dereference is cheap and infallible: it borrows a field, never polls, submits or waits. Do not implement `DerefMut`; issuance status must not be rewritten independently of the scheduling attempt. Borrowed `Result` methods are available, while consuming methods such as moving out an error are not supplied by dereferencing alone. The token is consumed by awaiting; it is not itself a `Result` supporting `?`. Document issuance-only inspection explicitly so `is_ok()` is not mistaken for a completed delivery check. See Rust's [Deref](https://doc.rust-lang.org/std/ops/trait.Deref.html) and [IntoFuture](https://doc.rust-lang.org/std/future/trait.IntoFuture.html) contracts.

All scheduled methods are ordinary `fn` returning `Completion<T, E>`, not lazy `async fn`. Inputs are owned, with the executor's required `Send` and lifetime bounds. Admission snapshots mutable span content under a short lock, registers its outcome atomically before publication and returns without waiting for network preparation, batching, retries or delivery. Queue overload/closure is stored immediately as the token's admission error, with no accepted work; do not hide overload behind unbounded tasks waiting for capacity.

Polling `Completion` first checks the stored issuance result, then reads/waits on its existing response receiver. It never initiates execution, chooses a runtime or resubmits a command. Dropping the token or its awaiting future does not cancel accepted work. Keep producer-side tracking alive independently of the observer and preserve background diagnostics and lifecycle failure reporting even if nobody awaits the token. An admission-accepted operation can still fail later; acceptance is not a delivery guarantee. Lifecycle no-ops such as ignored post-finish mutations have an explicit outcome rather than a fabricated backend receipt.

Use one input struct with `Default` for optional arguments instead of dynamic argument normalization. Required values stay required through constructors or validation; do not invent meaningless defaults. Local accessors such as context/permalink derivation remain ordinary synchronous functions when they need no scheduled work.

Reads, resource creation, API requests, evals and lifecycle calls all attempt submission at call time and return the same completion token shape, including an inspectable issuance result. Actual values/errors are retrieved by awaiting it. Fallible pagination exposes eager page requests and an async stream adapter which requests subsequent pages as consumed. There is no token blocking method or full-operation blocking exception. Synchronous callers may inspect the stored result and retain or discard tokens while accepted work runs; result-dependent composition uses an async context.

### Execution context and explicit runtime selection

```rust
pub struct ClientConfig {
    pub runtime: Option<tokio::runtime::Handle>, // None = internal; applies to both call styles
    // Authentication, endpoint and reliability settings.
}

// Convenience only: equivalent to Handle::try_current().ok().
// No automatic call to this helper during construction, polling or submission.
pub fn current_runtime() -> Option<tokio::runtime::Handle>;

struct ExecutionBinding {
    selected: SelectedRuntime,
    context: OnceExecutionContext,
}
enum SelectedRuntime {
    External(tokio::runtime::Handle),
    Internal(Arc<LazyRuntimeHost>),
}
struct ExecutionContext {
    runtime: tokio::runtime::Handle,
    services: ResourceServices,
    writes: Arc<LazyWriteService>, // queue/processor start only for SDK ingestion
    lifecycle: Arc<Lifecycle>,
    // Transport pools, timers and tracked worker handles are owned here.
}
impl ExecutionBinding {
    fn ensure_started(&self) -> Result<(), ScheduleError>;
    fn spawn_response<T, E, F, Fut>(
        &self, sender: CompletionSender<T, E>, operation: F,
    ) -> Result<(), ScheduleError>
    where F: FnOnce() -> Fut + Send + 'static,
          Fut: Future<Output = Result<T, E>> + Send + 'static,
          T: Send + 'static, E: From<ExecutionError> + Send + 'static;
}
impl ExecutionContext {
    async fn initialize(deps: ContextDependencies) -> Result<Self, InitError>;
    fn start_worker(&mut self, processor: DeliveryProcessor) -> Result<(), InitError>;
}
struct RuntimeHost { /* owns and continuously drives the internal Runtime */ }
```

```rust
// Default remains usable from synchronous code without Tokio setup.
let client = Client::new(ClientConfig { runtime: None, ..Default::default() })?;

// Explicitly opt into the current runtime. Outside Tokio this returns None.
let client = Client::new(ClientConfig {
    runtime: current_runtime(),
    ..Default::default()
})?;

// Or select a particular runtime directly.
let client = Client::new(ClientConfig {
    runtime: Some(runtime.handle().clone()),
    ..Default::default()
})?;
```

Choose `External` versus `Internal` from configuration at construction; start its context initialization once on the first submission, without waiting for a token to be polled. `None` always means the internal runtime, including inside an existing Tokio runtime. The helper captures the current context only where the user calls it. It returns an `Option<Handle>` directly, so outside Tokio it naturally selects the internal default. No ambient-runtime lookup occurs during submission or token observation. Direct API clients accept the same runtime option; `client.api()` inherits its SDK client's binding.

Every method issues a token and schedules its operation on the same execution context. The ingestion path performs bounded queue admission; a response task is spawned directly for reads/API calls. Both record immediate setup/scheduling success or failure before returning the token. Awaiting observes that failure or the operation's actual completion. Polling on another runtime never changes where work runs. An explicit runtime is honored whether or not the caller awaits; there is no separate sync context or second background processor for mixed use.

Initialization runs on the selected handle and creates runtime-bound transport, services and timers there. Retain a tracked initialization task/shared result so concurrent first calls start exactly one context and processor. On first ingestion use, the processor is automatically spawned with that same `handle.spawn(processor.run(...))`; use its handle for operation tasks and `spawn_blocking` callbacks as well. Do not use an ambient `tokio::spawn` from an arbitrary caller, construct runtime-bound I/O on the calling thread before scheduling, or require users to spawn `client.worker()` manually. For ingestion, install the bounded ingress queue and outcome registry synchronously, then spawn initialization without requiring any token to be polled. Response tasks use their own sender/receiver pairs and task supervision, never that ingress queue. Starting a read/direct API task must not start a log processor unless ingestion is actually used. Commands admitted during initialization remain bounded in that queue. Deferred login and network setup run as accepted work. An initialization task that fails completes affected tokens with an execution error and closes admission rather than stranding accepted work. Failure to arrange initial scheduling produces a token storing an admission error and no accepted command. Submission does not wait for worker readiness or HTTP completion.

The client owns its ingestion queue/worker, response-task supervision, outcome tracking and shutdown through its context; runtime ownership stays distinct. An explicit handle does not keep its caller-owned runtime alive. The caller must keep that runtime alive and progressing, including after acceptance while work is pending. A current-thread runtime progresses only while driven. A caller that explicitly selects it and then blocks its only driving thread may prevent initialization or processing from progressing despite successful queue acceptance. This is the caller's responsibility; the SDK does not silently switch to the internal runtime or start another driver. The internal default retains and continuously drives its actual runtime owner.

No tasks, workers, pools or partially polled I/O migrate between runtimes. Binding is fixed for the client's lifetime; a stopped explicit runtime produces unavailable/failed completion instead of fallback. Track worker exit and complete pending tokens with truthful runtime/worker-loss errors where observable, rather than leaving orphaned waiters. Awaiting a token only observes its outcome and dropping it does not cancel accepted work.

Keep admission bounded, distinguish queue acceptance from completed I/O, reserve lifecycle/control capacity and reject overload truthfully rather than accumulating unbounded pending tasks. Submission uses immediate bounded admission rather than `block_on`; token observation has no blocking helper. Document the explicit-runtime progress requirement without adding runtime detection or changing execution sources. SDK callback scheduling uses the selected context's blocking pool so callbacks can dispatch logging without occupying the delivery worker.

A standalone plain API client uses the same binding and completion rules to schedule one-attempt endpoint tasks, without an SDK log processor. SDK shutdown stops and drains its own worker/tasks but never shuts down a caller-owned runtime. Internal runtime ownership is released only after accepted work and cleanup; worker references must not keep their own host alive forever, and final cleanup must not join or drop a runtime from its own runtime thread.

Tokio references: [current handle and spawning onto a selected runtime](https://docs.rs/tokio/latest/tokio/runtime/struct.Handle.html), [runtime ownership](https://docs.rs/tokio/latest/tokio/runtime/struct.Runtime.html#sharing), [bridging and current-thread progress](https://tokio.rs/tokio/topics/bridging).

### Session and resource services

Keep the one-attempt HTTP interface small. The transport factory applies TLS/proxy/timeout/User-Agent configuration; endpoint clients translate requests and responses. Resource services own resolution and replay policy, and invoke the external retry executor only when the operation is safe to repeat.

```rust
struct DatasetService {
    api: DatasetEndpoint,
    registry: Arc<ResourceRegistry>,
    retry: Arc<RetryExecutor>,
}
impl DatasetService {
    async fn resolve(&self, scope: &CredentialScope, selector: DatasetSelector)
        -> Result<DatasetState, ApiError>;
    async fn create(&self, scope: &CredentialScope, options: CreateDataset)
        -> Result<DatasetState, ApiError>;
    async fn fetch_page(&self, scope: &CredentialScope, query: RecordQuery)
        -> Result<RecordPage, ReadError>;
    async fn insert(&self, scope: &CredentialScope, id: DatasetId, rows: Vec<RecordInput>)
        -> Result<InsertResult, ApiError>;
}

struct DatasetState {
    scope: CredentialScope,
    selector: DatasetSelector,
    service: Arc<DatasetService>,
    // Resolution is shared by clones through ResourceRegistry.
}
```

`api` endpoint implementations perform one attempt through `HttpTransport`. `DatasetService::insert` performs the resource operation on its execution context; it does not secretly enqueue tracing work. `RecordPage` carries records plus the next cursor, so page operations and stream adapters reuse one fallible pagination implementation. `ExperimentService` uses the same composition pattern for approved experiment operations; it never constructs fallback IDs on failure.

Name/ID resolution cache keys include credential scope, organization and endpoint as required. Resource handles share resolved state through `Arc`; a failed initializer remains an error and can be retried under policy. Credentials are redacted in debug/error output. There is no independent cache recreated by cloning a handle.

### Admission and delivery orchestration

For SDK ingestion, separate synchronous issuance from async delivery observation at one queue-admission boundary. This section describes log delivery, not scheduling of reads or other response tasks. Registration of completion tracking and publication to the queue must be atomic from the worker's perspective: a fast worker cannot finish before its outcome has an owner.

```rust
enum WriteCommand {
    SpanUpdate { scope: CredentialScope, target: Destination, update: SpanUpdate },
    // Add approved record/feedback commands as distinct variants, not fake spans.
}
struct WriteService { queue: AdmissionQueue, outcomes: Arc<OutcomeTracker> }
impl WriteService {
    fn enqueue(&self, command: WriteCommand, sender: CompletionSender<DeliveryReceipt, WriteError>)
        -> Result<(), EnqueueError>;
}

struct DeliveryProcessor {
    queue: QueueReceiver,
    preparer: CommandPreparer,
    merger: EventMerger,
    encoder: RowEncoder,
    batcher: Batcher,
    batch_encoder: BatchEncoder,
    capabilities: Arc<CapabilityProvider>,
    uploader: UploadCoordinator,
    outcomes: Arc<OutcomeTracker>,
    diagnostics: DeliveryDiagnostics,
}
impl DeliveryProcessor {
    async fn run(self, stop: ShutdownSignal) -> ProcessorExit;
    async fn process(&self, accepted: Vec<AcceptedCommand>) -> BatchOutcome;
}
```

`run` owns the timer/flush/shutdown loop. `process` resolves commands, groups compatible routes, merges rows, encodes them, partitions batches and delegates upload. Original acceptance IDs travel with merged rows and split batches. `OutcomeTracker` records terminal outcomes for those IDs, while diagnostics emit useful context. Preparation failures terminate the affected IDs without preventing independent groups from progressing.

The worker delegates algorithms to narrow collaborators. Tests primarily exercise the real async execution pipeline with controlled dependencies; focused processor tests and the production automatic loop cover scheduling invariants.

```rust
impl CommandPreparer {
    async fn prepare(&self, command: AcceptedCommand) -> Result<PreparedRow, PrepareError>;
}
impl EventMerger {
    fn merge(&self, rows: Vec<PreparedRow>) -> Vec<TrackedRow>;
}
impl RowEncoder {
    fn encode(&self, row: TrackedRow) -> Result<EncodedRow, EncodeError>;
}
impl BatchEncoder {
    fn encode(&self, rows: Vec<EncodedRow>) -> Result<EncodedBatch, EncodeError>;
}
```

`TrackedRow` and `EncodedBatch` retain original-operation membership. Any encoding failure is converted back to delivery failures for the affected members, not merely logged and forgotten.

### Upload composition and generic mechanisms

```rust
impl RetryExecutor {
    async fn run<T, E, F, Fut>(
        &self,
        policy: &RetryPolicy,
        budget: &mut RetryBudget,
        operation: F,
        classify: impl Fn(&E) -> RetryDecision,
    ) -> Result<T, RetryFailure<E>>
    where F: FnMut(Attempt) -> Fut, Fut: Future<Output = Result<T, E>>;
}
impl Batcher {
    fn partition<T>(
        &self, items: Vec<T>, limits: BatchLimits,
        weight: impl Fn(&T) -> usize,
    ) -> BatchPartition<T>;
}
enum UploadPlan {
    Direct(EncodedBatch),
    Overflow(EncodedBatch),
    Split(Vec<EncodedBatch>),
}
impl UploadPlanner {
    fn plan(&self, batch: EncodedBatch, capabilities: &Capabilities)
        -> Result<UploadPlan, PlanError>;
}

struct UploadCoordinator {
    planner: UploadPlanner,
    logs: Logs3Transport,
    objects: ObjectStoreTransport,
    retry: Arc<RetryExecutor>,
}
impl UploadCoordinator {
    async fn upload(&self, route: &UploadRoute, batch: EncodedBatch,
        capabilities: &Capabilities, budget: &mut RetryBudget) -> BatchOutcome;
}
impl Logs3Transport {
    async fn post(&self, route: &UploadRoute, body: Bytes) -> Result<(), UploadError>;
    async fn request_overflow(&self, route: &UploadRoute, input: OverflowInput)
        -> Result<OverflowGrant, UploadError>;
}
impl ObjectStoreTransport {
    async fn upload(&self, grant: &OverflowGrant, body: Bytes)
        -> Result<(), UploadError>;
}
```

`Logs3Transport` is a thin internal adapter over one-attempt logs3 endpoint methods, not another HTTP implementation. The coordinator asks the planner for direct/overflow/split work, then passes each one-attempt transport operation to `RetryExecutor`. Overflow performs grant → object upload → reference commit; the reference commit uses `Logs3Transport::post`. Retain the grant and successful stage state while retrying. A 413 returns control to replanning. `ObjectStoreTransport` receives only signed grant information and never the Braintrust credential scope. `BatchOutcome` preserves successful and failed operation members instead of discarding partial success behind one error.

`RetryExecutor` owns a clock/sleeper dependency; `RetryBudget` carries deadline/cancellation and remaining attempts. The classifier and policy are supplied by the protocol/resource caller. `BatchPartition` identifies oversized items separately from compliant groups. Neither mechanism knows how to create a span, perform HTTP, or schedule background dispatch.

`Logs3BatchUploader` becomes a compatibility delegate to the SDK upload coordinator, then is removed or renamed after consumer migration. The plain public API client supports manual ingestion with one-attempt methods; SDK ingestion adds reliability through this coordinator. Neither path introduces a public manual-delivery mode for the background processor.

### Retry and batching contracts

The generic retry executor takes an operation factory, retry policy, error/result classifier, clock/sleeper, and cancellation/deadline context. It does not inspect Braintrust rows or choose upload stages. Classification belongs to the caller: transport failures, 429 and eligible 5xx may retry; invalid input and authentication rejection normally do not. Respect supported server retry hints. A 413 is a replan signal, not repeated submission of identical bytes until retries run out.

Policies must encode replay safety. Do not automatically retry non-idempotent experiment creation just because it shares the HTTP transport. Preserve stable row identifiers for retryable ingestion. Network interruption after a server commit may have an unknown outcome; report that uncertainty rather than promising exactly once.

Prevent layered retry multiplication: each request is attempted once by the transport and repeated only by the external executor. A multi-stage upload shares a bounded overall deadline/budget, with explicit allocation to stages. Retrying the reference commit does not unnecessarily re-upload an already successful object. An expired grant is handled by upload replanning, not the generic retry mechanism.

The batcher is deterministic and generic over weighted items. Protocol adapters supply accurate measurements, compatibility groups, count limits and byte limits. Batch encoding verifies the final body size. One oversized row is identified explicitly for overflow or an error; it is not silently considered compliant. Separate the dispatch timer from grouping: `DeliveryProcessor` decides when to drain, `Batcher` decides how to group.

Never merge or batch across incompatible endpoint, organization, credential scope, or destination contexts. Preserve order for operations affecting the same record; delete, replacement and merge are not freely commutative. Acknowledgments must survive coalescing and splitting: multiple accepted operations can correspond to one merged row, and one planned batch can become multiple requests. Mark only confirmed members successful; retain successful outcomes when a sibling request fails.

## Delivery, errors, and lifecycle

### Ingestion operation flow

```text
log method call + owned input + client's selected execution binding
                 |
        create sender/completion pair + bounded queue admission
                 +------------------> return token with stored issuance Result
                 |                               |
        context-owned processor            optional .await
             prepare / resolve                   |
       group / merge / encode / batch            |
            UploadCoordinator                    |
     plan -> retry executor -> one attempt       |
                 |                               |
         OutcomeTracker completes --------------+
```

Admission is attempted during the synchronous method call, before its token is returned. Not awaiting or dropping its token does not retract accepted work or cancel a shared batch; the outcome remains observable through lifecycle reporting. Enqueue rejection must not partially publish a batch of commands presented as one atomic admission. Document bulk operations that intentionally allow partial acceptance.

`SpanState` produces an immutable snapshot/patch under a short lock; no network wait occurs while that lock is held. An end timestamp is set once. Admission rejection must leave the end command retryable without changing that timestamp. Repeated accepted end calls do not enqueue duplicate completion commands. Concurrent end calls can observe the same terminal outcome. Follow Ruby’s finished-span behavior: ignore later handle mutations with a diagnostic; repeated finish does not change timestamps; finishing a parent does not finish children. Existing explicit row-update/feedback APIs remain separate operations, not a way to reopen a finished live span.

### Errors and outcomes

| Error/result | Meaning |
| --- | --- |
| `ValidationError` / `ConfigError` | Locally invalid data or settings; no delivery claim |
| `ScheduleError` | Immediate setup/task-scheduling failure, independent of any ingestion queue |
| `EnqueueError` | SDK ingestion rejected admission: closed, capacity exhausted, invalid scope or unavailable worker |
| `ApiError` / `ReadError` | Actual resource request failure, with operation/status/source context |
| `DeliveryError` | Preparation, encoding, protocol or transport failure for accepted work, with stage and retry context |
| Operation-specific `E` (for example `WriteError`) | Stage-specific issuance/admission, execution or operation failure; shared by stored issuance and awaited result |
| `Completion<T, E>` | Stored issuance `Result<(), E>` plus an always-present response receiver; dereference borrows issuance, awaiting yields the method's `Result<T, E>` |
| `DeliveryReceipt` | Backend acknowledgment for an effective update, or an explicitly identified lifecycle no-op |
| `FlushReport` | Completed barrier coverage, counts and delivery failure summaries |
| `ShutdownReport` | Drain outcome plus any unresolved/abandoned work after deadline |
| `EvalError` / structured case outcomes | Setup/run failure or per-task/per-scorer failure, distinct from recording failures |

The stored issuance `Result` cannot contain future operation failures. Accepted calls to flush/shutdown schedule lifecycle work and return their token immediately; awaiting it yields the report. Diagnostics expose background failures independently. Awaiting a write includes its delivery outcome; awaiting flush also covers accepted work whose tokens were discarded or whose observers stopped waiting.

Preserve underlying error sources and operation identity where useful. Redact credentials and signed URLs. Retain preparation/batch diagnostics when adding typed errors. Do not turn HTTP rejection into configuration failure or EOF. Do not report registration failure as a fabricated successful resource.

### Barriers and shutdown

```rust
struct Barrier { through: OperationId }
struct Lifecycle {
    admission: Arc<AdmissionState>,
    outcomes: Arc<OutcomeTracker>,
    worker: WorkerControl,
}
impl Lifecycle {
    fn capture(&self) -> Barrier;
    async fn flush(&self, barrier: Barrier) -> Result<FlushReport, FlushError>;
    async fn shutdown(&self, deadline: Deadline)
        -> Result<ShutdownReport, ShutdownError>;
}
impl OutcomeTracker {
    fn complete(&self, members: &[OperationId], outcome: DeliveryOutcome);
    async fn wait_through(&self, barrier: Barrier)
        -> Result<FlushReport, FlushError>;
}
```

`Client::flush()` immediately captures an ordered watermark, admits a control command and signals its context's worker. The returned token stores control admission status; on acceptance, completion covers preparation failures and in-flight requests. On rejection it stores the error without scheduling a flush, and awaiting returns that error immediately. The flush runs even if its token is never awaited; awaiting retrieves the report. Shutdown closes admission, captures and drains the final barrier, then stops workers. An individual completion token observes its own shared result, so repeated accepted span-end calls can observe one result without issuing another command.

Admission and barrier capture share an ordering boundary. A flush captures an accepted-work watermark and completes only when every operation through it reaches a terminal outcome, including currently in-flight batches, preparation failures, and work already automatically drained. Later submissions must not postpone that barrier indefinitely. Use terminal tracking rather than an empty-queue check; a queue can be empty while HTTP is in flight.

Background failures remain available for explicit reporting independently of per-write async waiters. The final section compares reporting windows with lifetime counters and recommends windows. Do not retain historical payloads merely to report aggregate failures.

Client flush covers queued SDK work accepted in its session through either call style. Its lifecycle coordinator captures an ordered watermark and signals the worker on the client's single execution context. Barrier processing stays on that selected runtime; it never restarts an unavailable runtime or moves work elsewhere. Direct API requests complete on their own tokens and are not SDK flush obligations. Expose flush at the client level; avoid a resource method named `flush` that unexpectedly drains unrelated resources. A recorded eval may use an internal barrier for only its recording operations.

Shutdown moves `Open → Closing → Closed`, rejects new admission, drains accepted work within a bounded deadline, then stops workers and runtime ownership cleanly. Concurrent shutdown calls share completion. Timeout reports unresolved work; it does not claim unsent rows were delivered. Drop is best effort and bounded. No persistence or crash-recovery guarantee is introduced by an in-memory queue.

## Evaluation architecture

Separate reusable definitions, normalized run context, execution and results. One public eval method schedules work immediately on the bound executor and returns a completion token. Local execution has no required Braintrust client, login, network access, or recorder.

Use boxed futures at the callable/source boundaries that need runtime-selected implementations. Keep scheduling and result aggregation concrete. The sync and async callable adapters implement these same contracts; a synchronous callback runs through the managed blocking executor, never inline on the delivery worker.

```rust
pub trait Task<I, O>: Send + Sync {
    fn run<'a>(&'a self, input: &'a I, context: &'a TaskContext)
        -> BoxFuture<'a, Result<O, TaskError>>;
}
pub trait Scorer<I, O>: Send + Sync {
    fn name(&self) -> &str;
    fn score<'a>(&'a self, args: ScorerArgs<'a, I, O>)
        -> BoxFuture<'a, Result<Vec<Score>, ScorerError>>;
}
pub trait CaseSource<I, O>: Send {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Case<I, O>>, SourceError>>;
    fn provenance(&self) -> Option<DatasetVersionRef>;
}

pub struct Evaluator<I, O> {
    task: Arc<dyn Task<I, O>>,
    scorers: Vec<Arc<dyn Scorer<I, O>>>,
}
struct EvalPlan<I, O> {
    definition: Arc<Evaluator<I, O>>,
    source: Box<dyn CaseSource<I, O>>,
    options: ValidatedEvalOptions,
    recorder: Option<Box<dyn EvalRecorder<I, O>>>,
    reporter: Box<dyn EvalReporter>,
}
impl<I, O> EvalPlan<I, O> {
    fn validate(request: EvalRequest<I, O>) -> Result<Self, EvalConfigError>;
}
```

### Closures as tasks and named scorers

Implement the callable traits for matching functions/closures directly. A task is a repeatedly callable function with the task signature; a scorer is a `(name, function)` tuple. Custom structs can still implement either trait when they need domain-specific state or behavior. Ordinary closures should not require public `AsyncFnTask`/`AsyncFnScorer` wrapper structs.

```rust
impl<I, O, F> Task<I, O> for F
where
    F: Send + Sync,
    F: for<'a> Fn(&'a I, &'a TaskContext)
        -> BoxFuture<'a, Result<O, TaskError>>,
{
    fn run<'a>(&'a self, input: &'a I, context: &'a TaskContext)
        -> BoxFuture<'a, Result<O, TaskError>>;
}

impl<I, O, N, F> Scorer<I, O> for (N, F)
where
    N: AsRef<str> + Send + Sync,
    F: Send + Sync,
    F: for<'a> Fn(ScorerArgs<'a, I, O>)
        -> BoxFuture<'a, Result<Vec<Score>, ScorerError>>,
{
    fn name(&self) -> &str;
    fn score<'a>(&'a self, args: ScorerArgs<'a, I, O>)
        -> BoxFuture<'a, Result<Vec<Score>, ScorerError>>;
}
```

`Task::run` delegates to the closure. Tuple `name()` borrows `self.0.as_ref()` and `score()` delegates to `self.1(args)`, so both `&str` and `String` names work. The evaluator accepts trait implementations and erases them internally into its existing `Arc<dyn Task<...>>`/`Arc<dyn Scorer<...>>` fields. Validate scorer names and duplicates when building a run plan, regardless of how the scorer was supplied.

Use `Fn + Send + Sync` because a definition is reused across concurrent cases; a function that consumes its captured state once is not a reusable task. The `for<'a>` bounds permit each returned future to borrow that invocation's input/context or scorer arguments. Box the future at this boundary so its `Send` guarantee and lifetime remain explicit and the traits remain object-safe. A plain closure returning an arbitrary unboxed async block is not automatically this exact shape: contextual return coercion, a return annotation or a small inference helper can supply `BoxFuture`. Helpers can remain simple functions rather than new wrapper types. Captured shared state can use `Arc`, cloned into each invocation's future as needed. Do not require `I` or `O` to implement `Clone` merely to adapt a callable.

Do not add competing blanket implementations for both synchronous `Fn -> Result` and asynchronous `Fn -> Future` on every `F`: those implementations overlap. Keep the async callable shape canonical and use a small explicit blocking-callback adapter for synchronous functions/closures. That adapter owns scheduling onto the context's managed blocking pool; it must not execute a blocking callback inline on the delivery worker. This preserves one eval engine and the same task/scorer result contracts.

Validation should include captured closures, functions borrowing inputs across an await, both borrowed and owned scorer names, custom trait implementations, concurrent reuse, duplicate-name validation and blocking callback scheduling. Compile the user-facing examples against the crate's MSRV so lifetime coercions and `Send` bounds are demonstrated rather than assumed. Rust's [Fn documentation](https://doc.rust-lang.org/std/ops/trait.Fn.html) describes the reusable call contract.

### Plan execution and recording

`EvalPlan::validate` checks concurrency, unique scorer names, source selection and recording configuration before work starts. `DatasetCaseSource` adapts `DatasetService::fetch_page` to the source trait and retains dataset version/origin. In-memory and iterator sources need no client. The public `Evaluator` has no runtime or mutable run result; the public run method captures the request and schedules plan validation/execution before returning its token.

```rust
struct EvalExecutor { case_runner: CaseRunner, callbacks: BlockingExecutor }
impl EvalExecutor {
    async fn run<I, O>(&self, plan: EvalPlan<I, O>)
        -> Result<EvalResult<I, O>, EvalRunError<I, O>>;
}
impl CaseRunner {
    async fn run<I, O>(
        &self, case: Case<I, O>, definition: &Evaluator<I, O>,
        context: TaskContext, events: &mut CaseEventSink<'_, I, O>,
    ) -> CaseOutcome<I, O>;
}
impl<I, O> ResultAccumulator<I, O> {
    fn push(&mut self, index: CaseIndex, outcome: CaseOutcome<I, O>);
    fn finish(self, recording: RecordingStatus) -> EvalResult<I, O>;
}

trait EvalRecorder<I, O>: Send {
    fn record<'a>(&'a mut self, event: &'a EvalEvent<I, O>)
        -> BoxFuture<'a, Result<(), RecordingError>>;
    fn finish(&mut self) -> BoxFuture<'_, Result<RecordingStatus, RecordingError>>;
}
trait EvalReporter: Send {
    fn report(&mut self, event: &ProgressEvent) -> Result<(), ReportError>;
}
```

The run sequence is: validate plan → resolve an optional recording target → read cases with bounded concurrency → run each task and its scorers → accumulate identified outcomes → finish recording through its barrier → build the final result. `CaseEventSink` forwards typed case events to the recorder and progress events to the reporter; it does not own scheduling. Recording and reporting errors are tracked independently of task/scorer errors. `EvalRunError` retains partial results when a source or run-level failure aborts the run.

The recording adapter implements serialization only when a recorded run is constructed; local execution does not acquire serialization bounds just to satisfy an unused recorder. `TerminalReporter`, `JsonlReporter` and the CLI protocol reporter implement `EvalReporter`. The `bt` adapter builds options and reporters, invokes the same executor and translates the result into CLI status; it has no separate eval loop.

The public entry point follows the same operation contract:

```rust
pub fn run<I, O>(request: EvalRequest<I, O>)
    -> Completion<EvalResult<I, O>, EvalRunError<I, O>>;

// Optional settings use Default; task/source requirements stay explicit.
#[derive(Default)]
pub struct EvalOptions {
    pub concurrency: Option<NonZeroUsize>,
    pub experiment: Option<ExperimentSelector>,
    pub description: Option<String>,
    pub runtime: Option<tokio::runtime::Handle>, // client-free runs; otherwise inherit client binding
}
```

`eval::run(request)` immediately attempts submission on its selected runtime and returns a completion token; `eval::run(request).await?` propagates admission failure or execution/recording failure from that token. Local runs without a client accept the same optional runtime setting (internal by default) and require no credentials. Recorded runs retain their client's execution binding; callbacks and recorder stay in that run's context. Runtime ownership lasts through accepted work and finalization, even if a caller drops its token.

Keep `I`/`O` generics for user data. Require serialization at the recording boundary, deserialization at sources that need it, and equality only in scorers that compare values. Task/scorer errors should not require pretending a user's application error is an HTTP/SDK error.

Recorded evaluations resolve their experiment before executing cases. Each case has its own trace root attached to that experiment; task and scorer spans are children of the case. Avoid one enormous run-wide trace. A run summary is a separate evaluation result. Remote-parent support, if retained, must distinguish recording into an existing context from creating a new experiment.

The executor emits ordered/identified events to recorder and reporter through internal adapters. Define backpressure and failure behavior: recorder admission failure affects recording status; reporter failure must not silently corrupt computation. Avoid an unbounded generic event bus or workflow framework. Direct interfaces and a small event enum are sufficient. Recording completion is separate from computation completion; recorded run results include both, and the run entry point waits for its recording barrier before reporting recording success.

Source errors produce a failed/incomplete run with partial results preserved. Task failure skips dependent scorers; scorer failures remain associated with their names. Validate nonzero concurrency and unique scorer names. Preserve actual case IDs, span IDs, counts and error locations. Follow Ruby’s existing completion model: wait for scheduled case work and finalize spans on success/error. This refactor adds no public cancellation token or hard timeout for arbitrary user callbacks. Dropping a completion observer does not cancel an admitted run or undo accepted log writes; the executor still finalizes instrumentation through the normal processor.

Sync callbacks run on a managed blocking executor. Async callbacks run on the async executor. Both feed the same `CaseRunner` and result model; sync support must not require wrapping a separately implemented eval engine or constructing a second runtime.

## Module layout

```text
src/
  lib.rs                    # one public API and shared values
  client.rs                 # logical client ownership and operation construction
  completion.rs             # sender/completion pair, immutable issuance Result and result receiver
  api/                      # public endpoint views, DTOs, one-attempt implementation
  trace/                    # tracer/span, events, context and lifecycle state
  datasets/                 # selectors, resource operations and pagination
  experiments/              # existing experiment capabilities and identity
  eval/                     # definitions, executor, cases, recorder, reporters
  internal/
    session/                # resolution and immutable credential scopes
    retry/                  # generic retry executor and policies
    batching/               # generic weighted grouping
    delivery/               # admission, processor, upload orchestration, outcomes
    wire/                   # row conversion and propagation codec
    runtime/                # internal host, explicit binding, context ownership
  integrations/             # existing provider and CLI adapters
```

This maps implementation ownership, not a requirement to split every struct into a file. Shared input/result values have one definition. Do not create empty modules for missing features or parallel sync/async implementations. Keep existing streaming behavior behind an adapter until its dedicated decomposition phase.

## Testing the shared execution pipeline

Test the actual async services and processors with substituted I/O, time and scheduling. Submission and observation should be thin enough that the same behavior tests cover eager execution, synchronous acceptance and awaited completion. There is no second resource API to unwrap; test-only access exposes execution context and outcome tracking where necessary.

```rust
#[cfg(test)]
impl Client {
    pub(crate) fn with_test_dependencies(config: ClientConfig, deps: TestDependencies)
        -> Result<(Self, TestDriver), InitError>;
    pub(crate) fn context_for_test(&self)
        -> Result<Arc<ExecutionContext>, RuntimeError>;
}
impl TestDriver {
    async fn process_until_idle(&mut self) -> Result<(), TestError>;
}
```

`TestDependencies` supplies fake HTTP and time; `TestDriver` can step delivery deterministically on its owning context. Tests may call a method synchronously and later await its returned token to validate the actual result. Private hooks exist only in crate unit tests. Cargo integration tests use public operations, the direct API client, mock HTTP assertions and readback helpers. Do not publish a test-mode feature or manual-delivery switch.

Validation layers:

- **Direct API:** one request per admitted operation, typed errors/payloads, no SDK retry or log processor, explicit credentials/routing and independent manual ingestion.
- **Eager submission:** method calls schedule writes, reads, eval and lifecycle work before returning, without awaiting or polling tokens. Verify automatic progress with a retained unpolled token and after token drop; awaiting never duplicates submission. Rejection returns a token with stored `Err` and no accepted queue work; borrowed inspection does not consume the error and awaiting yields it immediately without requiring `E: Clone`. Accepted tokens await the actual result. Verify inspection remains issuance-only after a later operation failure, and awaiting never changes issuance status or submits again. Saturation rejects without unbounded staging tasks; initialization failures terminate affected tokens.
- **Completion routing and ownership:** construct sender and receiver together; prove the receiver is present even on immediate issuance failure, and that awaiting returns that original error without cloning. A read/direct API task receives its own sender and returns its normal response without touching the log queue, batcher or uploader. Ingestion tracking retains senders through merge/split/retry and sends only its final delivery result. Cover sender loss, receiver drop, early response before token return and fan-out for shared lifecycle outcomes.
- **Runtime binding:** `None` uses internal even with an ambient runtime; `Some(handle)` uses that runtime for both call styles, workers, HTTP, timers and callbacks. Verify `current_runtime()` inside/outside Tokio, unawaited concurrent submissions start one worker, observer runtime does not affect routing, runtime-loss errors, explicit-runtime progress requirements, bounded admission and no nested `block_on`.
- **Resource behavior:** immutable scopes, clone identity, truthful initialization/pagination errors, full-delivery awaits, merge/order/overflow correctness and acknowledgment membership across both paths.
- **Mechanisms:** fake-time retry budgets/deadlines, final body size, oversized rows, 413 replanning and overflow stage reuse.
- **Lifecycle:** barriers covering both call styles on the same context, retained failures, in-flight work, stop/drain, closing rejection, runtime cleanup and Ruby-compatible finished-span behavior.
- **Evaluation:** closure/function tasks, named tuple scorers with borrowed/owned names, borrowed async arguments, concurrent reuse and blocking adapters; local credential-free runs, task/scorer/source errors, partial results, concurrency, recording/reporting failures, callback reentrancy and cleanup after observer drop.
- **Independent readback:** direct API setup/query/verification; eventual-visibility polling only in a bounded validation helper.
- **Release:** examples, MSRV/platform/feature checks, packaged contents and explicit migration for git-pinned consumers.

Keep automatic-worker tests in addition to stepped tests. Avoid fixed sleeps as synchronization. Reproduce regressions before fixing them. Similar Ruby/Rust examples guide design; no exhaustive feature-parity gate is required.

## Staged PR roadmap

Keep each PR centered on one problem and leave the project working at every layer. Short stacks can be developed above a stable foundation and merged bottom-up. Mechanical moves should not obscure behavioral fixes. Replace temporary adapters with delegates to one implementation and state their removal point.

| PR | Problem and implementation boundary | Completion evidence |
| --- | --- | --- |
| 0 | Finalize this implementation plan and example signatures | Record operation/token/runtime decisions and the remaining failure-reporting policy; no feature-parity inventory gate |
| 1 | Extract only required API types/interfaces from PR #63 | One-attempt endpoint requests, explicit configuration/credentials and public manual ingest/readback; no background login/retry in API state |
| 2 | Extract generic retries | Existing request paths use `RetryExecutor`; transport methods make one attempt; preserve then deliberately reconcile policy differences |
| 3 | Separate batching, encoding and upload planning | `Batcher`, encoders and `UploadPlanner` handle accurate body sizes, oversized rows and 413 replanning without owning HTTP |
| 4 | Consolidate both upload implementations | `UploadCoordinator` composes base API methods, signed upload and retries; preserve overflow stage state and partial success |
| 5 | Establish admission, completion and lifecycle | Atomic `WriteService` admission, `Completion`, automatic processors and barriers retain failures and support full-delivery awaits |
| 6 | Isolate existing resource/span behavior | Session/resource services share identity and truthful errors; typed commands replace synthetic span payload assumptions |
| 7 | Introduce eager submission and one completion type | Ordinary methods schedule immediately; tokens expose immutable issuance Results via Deref and eventual results via IntoFuture; one explicit-or-internal runtime binding and same-runtime worker ownership; no duplicate public API |
| 8 | Separate evaluation execution, recording and reporting | One executor behind eager eval submission and completion tokens; no extra cancellation framework |
| 9 | Simplify CLI adapters and prepare release | Fix local/list/output/completion behavior; remove old delegates after consumer migration; compile documentation and verify packaging |
| 10 | Decompose provider streaming separately | Provider decoder, incremental accumulator and tracing/iterator adapters with fixture coverage; preserve current behavior until this phase |

### PR 1 — Extract a narrow direct API client

This first layer exposes direct async endpoint calls while preserving the existing SDK. The final eager completion surface is introduced in PR 7; do not pull runtime hosting, completion tokens or SDK reliability into this transport extraction just to present the final facade early.

Inventory the precise request/response types and endpoint method signatures needed by current SDK call sites, manual ingestion and test readback. Extract only that subset from [PR #63](https://github.com/braintrustdata/braintrust-sdk-rust/pull/63), adapting it to the smaller API client design on the current base. Carry over an implementation helper only when the selected contract needs it; do not carry over modules, generated endpoint coverage, macros or session/background-login machinery by default. Keep retries, batching and lifecycle orchestration in the SDK. Validate selected payloads and errors with focused contract tests, preserving current TLS/proxy/User-Agent behavior. This is a selective extraction PR, not a cleanup, rebase or merge of the whole #63 branch.

### PRs 2–4 — Separate mechanisms, then unify protocol orchestration

Extract retries first without combining policy changes with code movement. Then extract generic grouping and final-body encoding, retaining operation membership in rows/batches. Finally route the queue and standalone uploader through one coordinator backed by the direct API. Confirm no retries multiply across layers, direct API calls still make one attempt, and successful overflow stages are reused. `Logs3BatchUploader` becomes a temporary compatibility adapter rather than an independent protocol implementation.

### PR 5 — Make admission and completion observable

Implement atomic admission/outcome registration, automatic scheduling and shared completion tokens. Delivery completion covers preparation, batching, retries and overflow; it must not wait indefinitely for a batch to fill. Flush captures accepted work including already drained and in-flight work. Preserve diagnostics and failure retention, finalizing the proposed reporting policy before implementing tracker semantics. Keep compatibility entry points until PR 7; replace `checked_delivery` with ordinary completion observation and direct API/manual-ingest operations. Migrate external WAL consumers deliberately.

### PR 6 — Isolate resource and span behavior

Move endpoints to the base API and resource policies/state into focused services. Fix invented experiment IDs, cached failure strings, clone registration resets, tenant state mutation, pagination EOF-on-error and inconsistent pending buffers. Preserve row updates and deterministic IDs independently of live-span finish. Normalize optional parameters with typed `Default` input structs, not a dynamic argument subsystem.

### PR 7 — Add eager submission and enforce runtime ownership

Expose one resource API of ordinary methods that immediately submit owned commands and return `Completion<T, E>` for every scheduled capability, including reads, API calls and lifecycle operations. The token stores an issuance `Result<(), E>` and an always-present one-shot receiver, exposes issuance through immutable `Deref`, and implements `IntoFuture<Output = Result<T, E>>` to propagate admission failure or observe accepted work; it has no blocking or scheduling method. Verify progress without polling, bounded rejection and cleanup after token drop. Remove superseded sync/async wrapper hierarchies and namespaces after migration. Implement `Option<Handle>` selection, an explicit `current_runtime()` helper and persistent internal ownership for the default. Create services, transports, processors and timers on one selected context shared by both call styles; never migrate work or infer a runtime from the call site. Add test-only dependency/context access and barrier coverage for mixed call styles. Document caller ownership/progress obligations when overriding the runtime. Validate callback reentrancy, overload, dropped observers and runtime shutdown. Plain synchronous binaries need no Tokio setup; actual eventual values are retrieved by awaiting tokens, with no blocking-result helpers.

### PR 8 — Rebuild evaluation around the async implementation

Implement reusable evaluator definitions, normalized plans, case sources, `CaseRunner`, result accumulation, recorder and reporter. Add the direct closure `Task` implementation and named-tuple `Scorer` implementation; retain only the explicit adapter needed for blocking callbacks. Move project/experiment setup into the correct services. Preserve local credential-free execution and dataset provenance. Task/source/scorer errors remain meaningful, recording success requires delivery completion, and presentation has no execution logic. Calling eval immediately schedules the executor on its selected context; the token stores admission status and carries the eventual run result for accepted work regardless of whether it is observed. Follow Ruby's wait-for-work/finalize-spans model without adding public cancellation policy questions.

### PR 9 — Release preparation

The CLI chooses options/reporters around the same async executor; local/list modes do not authenticate or submit, and output modes remain clean. Update existing workflows toward Ruby's familiar shapes where useful, without requiring new feature families. Verify actual head checks, supported platforms, MSRV, examples and packaged contents separately from local compilation. Keep release publication outside this refactor's authorization.

### PR 10 — Streaming decomposition

Keep this as the previously agreed separate phase. Retain compatibility adapters for current async and synchronous capabilities while earlier layers move. Then isolate provider parsing, normalized state, accumulation and instrumentation; test fragmented tool calls, usage-only chunks, unknown/malformed chunks, errors and early drop. Avoid rescanning all retained raw chunks where incremental state suffices. No separate streaming release-scope decision or expanded provider feature matrix is required now.

## Settled lifecycle behavior

- **Completion:** ordinary method calls attempt scheduling immediately and return a token storing issuance `Result<(), E>` and a receiver constructed together with its sender. Immutable dereference optionally exposes issuance status; awaiting returns admission failure or the accepted work's result. Awaiting only retrieves the eventual value/error; writes include full delivery. There is no separate operation type, token scheduling method or blocking wait method.
- **Public shape:** one root resource API with eager submission and completion tokens; no separate sync/async resource namespaces or full wrapper copies.
- **Runtime:** one binding per client: explicit `Some(handle)` or internal `None`, shared by both call styles and the background worker. No automatic detection. `current_runtime()` explicitly captures an optional current handle. Users selecting a runtime own its lifetime/progress obligations; no migration or fallback after binding.
- **Scope:** improve existing features and API familiarity. Feature gaps remain future-work notes in the companion reference, not tasks or release gates for this plan.
- **Finished spans:** follow Ruby's OpenTelemetry behavior: ignore post-finish handle mutations with diagnostics, keep repeated finish idempotent and leave child spans independent. Existing explicit row-update APIs remain separate. A failed final enqueue can be retried without changing the chosen end timestamp.
- **Shutdown:** follow the processor model Ruby uses: stop accepting work, drain/flush accepted work, stop the worker and release resources. Preserve the SDK's request/retry time bounds, report failures, and provide explicit lifecycle completion; final drop is best effort. Exact runtime/thread cleanup is a Rust implementation responsibility, not a new product policy.
- **Evaluation completion:** wait for scheduled work and finalize instrumentation on success/error, as Ruby does. No new public cancellation token or forced interruption of synchronous callbacks is introduced.
- **Streaming:** decomposition stays a later phase; preserving current behavior while refactoring does not need another scope question.

Reference evidence for these defaults: Ruby's `eval/runner.rb` finishes spans in `ensure`; `internal/thread_pool.rb` joins workers. Its OpenTelemetry SDK dependency 1.10.0 ignores attribute changes after finish and its batch processor stops, joins and force-flushes during shutdown. The existing Rust row-update/merge capability is distinct from mutating a finished live handle.

## Failure reporting to settle

There are two different observations: whether issuing/scheduling a call succeeded, and the result of executing it. For ingestion, successful issuance includes queue admission; reads and direct API calls instead use response-producing tasks outside the log pipeline. Synchronous admission observation can only report the first. Awaited completion tokens, completed flush/shutdown reports and diagnostics expose later outcomes. Automatic background delivery must not consume a failure before those observers can see it.

For example, ten ordinary log calls can return tokens whose stored admission results are all `Ok`, then nine writes deliver and one exhausts its retries. The tenth acceptance was still truthful. The next explicit flush completion should report the failed delivery, even if the background worker has already emptied the queue. Callers awaiting that token receive their delivery error directly. A direct API caller receives its one-attempt HTTP failure when awaiting its token; the stored synchronous issuance `Result` reports only setup/scheduling.

The remaining choice is how long explicit flush reporting remembers an already reported failure:

| Policy | Behavior after a failed flush | Tradeoff |
| --- | --- | --- |
| Reporting windows — recommended | A completed flush reports work since the previous completed flush; the next window can be healthy after recovery | Useful for periodic monitoring; callers must handle each report. Reporting a failure does not retry it or mean it later delivered. |
| Lifetime cumulative reporting | Every later flush still includes all earlier failures, even after recovery | Harder to miss history, but one old failure makes the client appear permanently unhealthy unless callers interpret counters/deltas. |

I recommend reporting windows with bounded representative error details and totals. Retain background failures until a completed explicit report covers them; timeout/unresolved work remains tracked. Concurrent flush calls should share an active barrier/report rather than race to consume failures. Async per-write errors remain part of the window summary so client-wide health does not depend on how each writer was called. Keep payload history out of the tracker and emit diagnostics regardless of whether a caller awaits a result.

```rust
pub struct FlushReport {
    pub delivered: u64,
    pub failed: u64,
    pub unresolved: u64,
}
pub enum FlushError {
    Admission(EnqueueError),
    Execution(ExecutionError),
    Delivery(FlushFailure),
}
pub struct FlushFailure {
    pub report: FlushReport,
    pub failures: Vec<DeliveryFailure>, // bounded sample, redacted
    pub details_truncated: bool,
}
```

Failed or unresolved delivery in the reporting range yields `FlushError::Delivery(FlushFailure)`; rejected admission is stored as `FlushError::Admission` in the token and is returned immediately when awaited. Success yields `Ok(FlushReport)` from the accepted token. Stored admission and awaited completion use the same `FlushError` type. A network failure with uncertain server commit is reported as uncertain, not automatically retried forever or called exactly-once delivery. These reporting fields and window semantics remain the one policy proposal to confirm before implementing the outcome tracker. They do not require a public delivery-mode switch or an event/callback framework.
