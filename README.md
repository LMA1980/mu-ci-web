# mu-ci-web

<p align="center">
  <a href="https://hook.lma1980.duckdns.org/jobs">
    <img src="https://hook.lma1980.duckdns.org/cards/LMA1980/mu-ci-web/card.svg" alt="mu-ci-web Factory Scorecard" width="520"/>
  </a>
</p>

<p align="center">
  <a href="https://hook.lma1980.duckdns.org/jobs"><img src="https://hook.lma1980.duckdns.org/badges/LMA1980/mu-ci-web/status.svg" alt="CI Status"/></a>
  <a href="https://hook.lma1980.duckdns.org/jobs"><img src="https://hook.lma1980.duckdns.org/badges/LMA1980/mu-ci-web/score.svg" alt="Quality Score"/></a>
  <a href="https://hook.lma1980.duckdns.org/badges/LMA1980/mu-ci-web/coverage.svg" alt="Coverage"/></a>
  <a href="https://hook.lma1980.duckdns.org/badges/LMA1980/mu-ci-web/benchmark.svg" alt="Benchmark"/></a>
</p>

______________________________________________________________________

WebAssembly queue monitoring, observability drill-down, and pipeline visualization frontend built with **[Leptos](https://github.com/leptos-rs/leptos) (CSR)** in the [`mu-ci`](https://github.com/LMA1980/mu-ci) platform for [`mu-ci-controller`](https://github.com/LMA1980/mu-ci-controller), operating alongside [`mu-console`](https://github.com/LMA1980/mu-console) (`https://console.lma1980.duckdns.org`).

______________________________________________________________________

## Key Features

- **Live Webhook Queue Monitor**:

  - Real-time queue depth and cluster worker lock visibility (`ClusterNodeLocksBar`, `node_status`).
  - Spotlight for executing builds with live elapsed timers and ordered FIFO pending queue with wait timers and position indicators (`#1`, `#2`, ...).
  - Coalesced events tracking showing when older queued commits were superseded by newer commits for the same repository and ref.
  - Global canary repository health badge (`CanaryStatusPill`) polling `GET /api/v1/canary/status` on [`mu-registry`](https://github.com/LMA1980/mu-registry).

- **Deep Job & Observability Drill-Down Views (`src/components/`)**:

  - Direct URL navigation (`#/jobs/{id}`), live build status banner, duration, commit SHA, trigger details, and embedded vector SVG scorecards (`/cards/{owner}/{repo}/card.svg`).
  - Submodules breakdown grid dynamically scaling across repository submodules (such as `jero-core`, `jero-engine`, `jero-manager`).
  - **DAG & Task Pipeline Tree (`PipelineTreeView`)**: Visualizes stage topology, dependencies, and typed tool tasks.
  - **W3C Distributed Trace Waterfall (`TraceWaterfallModal`)**: Renders per-stage and cross-service trace spans reconstructed from job telemetry.
  - **Hardware-Stratified Benchmark & Flakiness Analytics (`BenchmarkSparklinePanel`, `TestFlakinessHeatmap`)**: Tracks X100 vs. A100 vs. x86_64 micro-benchmark series and multi-run test flakiness.
  - **Sandbox Attenuation Telemetry (`MuIsolationTelemetryPanel`)**: Displays `KernelSandbox` / `HypervisorSandbox` profiles, memlock bounds, and egress policies.
  - Step-by-step ANSI log viewer (`ansi.rs`, `log_parser.rs`) parsing pipeline stages with exit codes, timing, search filter, and raw stdout/stderr output.

- **K3 Scorecard Theme**:

  - Dark mode aesthetic matching the GitHub native dark palette (`#0d1117`, `#161b22`, `#2ea44f`, `#da3633`, `#e3b341`, `#58a6ff`).

______________________________________________________________________

## Architecture & Integration

```text
                       [ GitHub Webhook / Developer Browser ]
                                        │
                                        ▼
                             [ Nginx Reverse Proxy ]
                            (hook.lma1980.duckdns.org)
                                  │           │
                  Static SPA      │           │ API, Webhook, Cards & Badges
                  (/, /style.css, │           │ (/webhook, /api/*, /queue,
                   /pkg/*.wasm)   │           │  /cards/*, /badges/*)
                                  ▼           ▼
                         [ mu-ci-web ]        [ mu-ci-controller ] ──> [ mu-registry ]
                         (Leptos CSR)         (CI Control Plane)       (/api/v1/canary/status)
```

- Nginx reverse-proxies `/api/*`, `/queue`, `/webhook`, `/cards/*`, and `/badges/*` to [`mu-ci-controller`](https://github.com/LMA1980/mu-ci-controller) (and canary status queries to [`mu-registry`](https://github.com/LMA1980/mu-registry)).
- Nginx serves the compiled Leptos WASM bundle from `/var/www/mu-ci-web` (staged atomically by `mu-cd-agent` per [`.mucd/mu-cd.yml`](.mucd/mu-cd.yml)).
- `mu-ci-web` (`https://hook.lma1980.duckdns.org`) operates side-by-side with [`mu-console`](https://github.com/LMA1980/mu-console) (`https://console.lma1980.duckdns.org`), sharing backend status/telemetry feeds from `mu-ci-controller`.
- Passing `?api=https://...` in the URL overrides the target API endpoint.

______________________________________________________________________

## Building & Verification

### Local Development with Trunk

```bash
# Build and serve locally with hot reloading
trunk serve
```

### Production Release Build

```bash
# Compile optimized WebAssembly release bundle to ./dist
trunk build --release
```

### Native Tests & Linter Hygiene

```bash
# Verify formatting, linter hygiene, and unit tests
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

______________________________________________________________________

## License

Licensed under the GNU Affero General Public License v3.0 (AGPL-3.0-only). See [LICENSE](LICENSE) for details.
