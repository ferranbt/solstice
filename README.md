# Solstice

Solstice is a VSCode extension for Solidity that adds debugging and execution tracing capabilities. It lets you trace and debug the execution of Solidity tests directly in your editor. The extension provides real-time visualization of contract state, memory, storage, and execution flow, making it easier to understand and troubleshoot smart contract behavior.

The language support in Solstice is forked from the [Solang VSCode extension](https://github.com/hyperledger-solang/solang/tree/main/src/bin/languageserver). This fork maintains all the language features from Solang while adding specialized debugging tools. Long-term, the plan is to migrate to the [Solar](https://github.com/paradigmxyz/solar) parser library when it's production ready.

## Quick Start

1. Install the VS Code Solstice extension from the [marketplace](https://marketplace.visualstudio.com/items?itemName=ferranborreguero.solstice-language-server)
2. Open any Solidity file to automatically activate the extension
3. Download the language server - A modal window will appear asking to download the Solstice language server from GitHub releases. Click "Download" to proceed
4. You're ready to go! The extension provides full Solidity language support and debugging capabilities

## Architecture

Solstice consists of two main components:

* **Language Server** (Rust): Handles Solidity parsing, analysis, debugging, and runs the [DAP server](https://code.visualstudio.com/blogs/2018/08/07/debug-adapter-protocol-website) for tracing
* **VS Code Extension** (TypeScript): Lightweight client that communicates with the language server via [LSP](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide) and launches DAP sessions

The extension is essentially a thin wrapper - all Solidity functionality and debugging capabilities live in the Rust language server.

## Development and Testing Locally

To develop and test Solstice locally, follow these steps:

1. Clone the repository
```
git clone https://github.com/ferranbt/solstice.git
cd solstice
```

2. Build the Rust server
```
cargo build
```

3. Build the VSCode extension
```
cd extension
npm run build
```

4. Open `Solstice` in Vscode and Open the `Run and Debug` tab. Execute the `Client + Server` configuration and open any Solidity project.

## Commands

Solstice provides two main commands:

### Server

```
$ cargo run -- server
```

Starts the Rust language server directly. This is what the VS Code extension calls internally to provide language features.

### Trace

```
$ cargo run -- trace --match-test <test-name> --match-path <path>
```

Traces the execution of Solidity tests locally without VS Code. This command is useful for debugging tests outside the editor or in CI environments. It provides detailed execution information, including contract state, memory, and storage at each step of the test execution.

Usage:
- `workspace`: The root directory of your Solidity project
- `test-name`: The name of the test to trace
- `match-path`: The path to the Solidity file containing the test
- `flamegraph`: Optional flag to generate a flamegraph of the execution
- `pprof`: Optional flag to generate a pprof profile of the execution
