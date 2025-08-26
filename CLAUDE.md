# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Solstice is a VSCode extension for Solidity that adds debugging and execution tracing capabilities. The project consists of two main components:

- **Language Server** (Rust): Handles Solidity parsing, analysis, debugging, and runs the DAP server for tracing
- **VS Code Extension** (TypeScript): Lightweight client that communicates with the language server via LSP and launches DAP sessions

## Architecture

### Core Components

- `src/lib.rs`: Main language server implementation with LSP handlers
- `src/debugger/`: Debugging infrastructure including DAP server and trace generation
- `src/builder.rs`: AST analysis and cache building for language features
- `src/symbol_indexer/`: Symbol indexing for cross-file references
- `src/forge/`: Foundry/Forge integration for building and testing
- `extension/`: TypeScript VSCode extension client

### Key Technologies

- Rust-based language server using `tower-lsp-server`
- Solang parser (forked version) for Solidity AST parsing
- REVM inspectors for EVM execution tracing
- Foundry/Forge for Solidity compilation and testing
- Debug Adapter Protocol (DAP) for debugging support

## Development Commands

### Building

```bash
# Build the Rust language server
cargo build

# Build the VSCode extension
cd extension && npm run build
```

### Development Tools

```bash
# Lint Rust code (fails on warnings)
just lint
# Or: cargo clippy -- -D warnings

# Check Rust code formatting
just check-format
# Or: cargo fmt -- --check

# Extension linting
cd extension && npm run lint

# Extension formatting
cd extension && npm run format:check
```

### Testing and Tracing

```bash
# Run all debugger traces
just trace all

# Run specific trace
just trace <trace_name>

# Fuzz testing for state
just fuzz-state

# Override test traces (updates expected output)
just override-traces
```

### Documentation

```bash
# Generate config documentation
just generate-docs

# Validate config documentation is up-to-date
just check-docs
```

## Running Tests

Use standard Rust testing:
```bash
cargo test
```

For tracing tests specifically:
```bash
cargo test --lib -- debugger::tracer::tests::test_debugger_traces --exact --show-output --nocapture
```

## Standalone Tracing

You can trace Solidity tests without VS Code:
```bash
cargo run -- trace --match-test <test-name> --match-path <path> [--workspace <workspace>] [--dump <output-file>] [--flamegraph <flamegraph-file>] [--pprof <pprof-file>]
```

## Configuration

The language server supports configuration through VSCode settings. Key settings are documented in `docs/configuration.md` and include:

- `solstice.inlayHints.closingBraceHints.enable`: Display closing brace hints (default: true)
- `solstice.inlayHints.closingBraceHints.minLines`: Minimum lines for brace hints (default: 25)
- `solstice.solsticePath`: Path to language server directory

## Language Server Capabilities

- Full Solidity language support (completion, hover, goto definition, references)
- Real-time diagnostics and error reporting
- Code formatting using forge-fmt
- Inlay hints for closing braces
- Code actions (SPDX license insertion, auto-imports)
- Code lens for running/debugging tests
- Dependency graph generation

## Development Notes

- The language server uses a forked version of Solang parser
- Debug traces are temporarily stored at `/tmp/debug_trace.json`
- The DAP server runs on port 50051 for debugging sessions
- Library remappings are auto-discovered from `lib/` directory
- Tests must have `test_` prefix to be recognized by code lens