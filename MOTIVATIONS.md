# Forgeclaw: Motivations

## What NanoClaw Proved

NanoClaw demonstrated that a single-process Node.js orchestrator could manage Claude agents in isolated Docker containers, route messages across multiple chat platforms, and provide per-group memory and session isolation. Over 2.5 weeks of intense development, it matured from a bare-metal prototype to a containerized, database-backed, CI-tested system with 360+ unit tests, comprehensive security (credential proxy, mount allowlists, IPC authorization), and a flexible skills-as-branches extension model.

NanoClaw proved the concept is viable. But it also revealed where the architecture breaks down.

## What's Wrong

### Single-Process Bottleneck

Everything runs in one Node.js process. If it crashes, all groups lose service. There's no clustering, no process isolation between subsystems, and no way to scale horizontally. A memory leak in the Discord channel handler takes down message processing for every group.

### File-Based IPC is Fragile

Containers communicate with the host by writing JSON files to a watched directory. `fs.watch()` has known platform quirks — dropped events, duplicate notifications, no ordering guarantees. A 5-second polling fallback catches what inotify misses, but that's 5 seconds of latency. Race conditions exist: files can be deleted between detection and processing. Output parsing relies on sentinel markers (`---NANOCLAW_OUTPUT_START---`) in stdout — if a marker splits across stream chunks, partial JSON gets parsed and data is silently lost.

### Global Mutable State Without Safety

Seven pieces of global mutable state (`lastTimestamp`, `sessions`, `registeredGroups`, `lastAgentTimestamp`, `channels`, `queue`, `pendingTailDrain`) are mutated from multiple async code paths with no synchronization. If `saveState()` fails or two mutations interleave, cursor corruption cascades — messages get replayed or skipped.

### Claude-Only

The entire system assumes Anthropic: the credential proxy injects a single Anthropic API key, the agent runner wraps Claude Code SDK, configuration references `ANTHROPIC_BASE_URL`, and per-group memory lives in `CLAUDE.md`. There's no path to supporting other model providers, no token budget management across providers, and no fallback chain when a provider is down or rate-limited.

### Container Spawn Overhead

Every message batch spawns a fresh Docker container: pull image, create container, start, compile TypeScript agent runner, initialize SDK, process messages, destroy. Session resume helps for follow-up turns, but cold starts are expensive. There's no container reuse, no warm pool, and no way to keep an idle container ready for the next message.

### No Event Bus

Subsystems communicate through direct function calls and closure-captured callbacks. `index.ts` (the composition root) owns all state and wires everything together. Adding a new cross-cutting concern (metrics, tracing, audit logging) requires modifying every subsystem. There's no way to subscribe to system events without coupling to the producer.

### Incomplete Abstractions

The container runtime is hardcoded to Docker CLI subprocess calls. Apple Container support exists as a skill branch with a different code path, not as an alternative implementation behind a shared interface. The database layer has a clean adapter interface, but queries use raw SQL strings with runtime type assumptions. Configuration mixes environment variables, JSON files, and hardcoded defaults across multiple modules.

## Why a Clean-Room Rewrite

These aren't bugs that can be fixed incrementally. The global mutable state, callback-based wiring, file-based IPC, and Claude-only assumptions are load-bearing architectural decisions. Refactoring them in place would touch every module while maintaining backward compatibility with a running production system. A clean-room rewrite lets us:

1. **Choose the right foundations**: Typed state machines instead of mutable globals. Event bus instead of callbacks. Structured IPC protocol instead of file watching. Compile-time checked SQL instead of string queries.

2. **Design for multi-model from day one**: Provider registry, per-group model assignment, token budgets, and fallback chains are first-class concepts, not retrofitted features.

3. **Integrate Tanren deeply**: Two container types (interactive agents and code creation/validation) with different lifecycle management. Self-improvement through Tanren dispatches. This is impossible to retrofit without rethinking the container architecture.

4. **Be compose-native**: Not "an application that happens to run in Docker" but "infrastructure that joins your compose stack as a peer service." Service discovery, network policies, health integration.

## Why Rust

**Type safety**: The state management bugs in NanoClaw (cursor corruption, untyped JSON parsing, `as any` casts) become compile-time errors. Typed state machines make invalid container transitions unrepresentable.

**Performance**: Zero-cost async with tokio. No garbage collector pauses during message processing. Native-speed JSON parsing with serde. Direct Docker Engine API communication via bollard (no subprocess spawning).

**Modular boundaries**: Cargo workspace with independent crates. Each subsystem compiles independently, has its own test suite, and exposes a typed public interface. Dependency cycles are compile-time errors.

**Ecosystem**: bollard (Docker API), sqlx (compile-time checked SQL), tokio (async runtime), serde (serialization), hyper (HTTP server) — mature, production-grade libraries that align with every major subsystem need.

## The Vision

Forgeclaw is a compose-native AI runtime. It joins your Docker Compose stack as a service — alongside your database, your application, your Tanren instance. It receives messages from communication channels (Discord, Telegram, Slack, or anything that implements the channel trait), routes them to intelligent agent containers running in isolated Docker environments, and manages the full lifecycle of those containers with typed state machines and structured IPC.

It speaks to any model provider — Anthropic, OpenAI, Ollama, local models — with per-group assignment, token budgets, and automatic fallback. It dispatches code work to Tanren for creation, validation, and auditing. It can improve its own capabilities by dispatching Tanren jobs against its own codebase.

It is infrastructure, not an application. It is a forge, not a chatbot.

## Name

Forgeclaw. Tanren (鍛錬) means "forge" or "temper" in Japanese — the engine that does the forging. Forgeclaw is the system that wields it. The name encodes the relationship: the forge's claw.

The `-claw` suffix connects to the lineage: NanoClaw → Forgeclaw. A clean-room rewrite, not a fork.
