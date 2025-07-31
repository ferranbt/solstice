/* --------------------------------------------------------------------------------------------
 * Copyright (c) Microsoft Corporation. All rights reserved.
 * Licensed under the MIT License. See License.txt in the project root for license information.
 * ------------------------------------------------------------------------------------------ */

import {
  workspace,
  ExtensionContext,
  window,
  CancellationToken,
  ProviderResult,
  debug,
} from "vscode";

import { WorkspaceFolder, DebugConfiguration } from "vscode";

import * as fs from "fs";
import * as Net from "net";
import * as vscode from "vscode";
import * as os from "os";
import * as path from "path";
import axios from "axios";
import { extract } from "tar";
import { execSync } from "child_process";
import { createMiddleware } from "./middleware";
import { Config } from "./config";
import { log } from "./log";

import {
  Executable,
  TransportKind,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  CodeAction,
  CodeActionResolveRequest,
} from "vscode-languageclient/node";

let client: LanguageClient;

const traceOutputChannel = window.createOutputChannel("Solstice");

export async function activate(context: ExtensionContext) {
  log.info("Activated");

  // register a configuration provider for 'mock' debug type
  const provider = new MockConfigurationProvider();
  context.subscriptions.push(
    vscode.debug.registerDebugConfigurationProvider("mock", provider),
  );

  const config = new Config();

  // register a dynamic configuration provider for 'mock' debug type
  context.subscriptions.push(
    vscode.debug.registerDebugConfigurationProvider(
      "mock",
      {
        provideDebugConfigurations(
          _folder: WorkspaceFolder | undefined,
        ): ProviderResult<DebugConfiguration[]> {
          return [
            {
              name: "Dynamic Launch",
              request: "launch",
              type: "mock",
              program: "${file}",
            },
            {
              name: "Another Dynamic Launch",
              request: "launch",
              type: "mock",
              program: "${file}",
            },
            {
              name: "Mock Launch",
              request: "launch",
              type: "mock",
              program: "${file}",
            },
          ];
        },
      },
      vscode.DebugConfigurationProviderTriggerKind.Dynamic,
    ),
  );

  // activate debug
  context.subscriptions.push(
    debug.registerDebugAdapterDescriptorFactory(
      "mock",
      new MockDebugAdapterNamedPipeServerDescriptorFactory(),
    ),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand(
      "extension.mock-debug.getProgramName",
      (_config) => {
        return vscode.window.showInputBox({
          placeHolder:
            "Please enter the name of a markdown file in the workspace folder",
          value: "readme.md",
        });
      },
    ),
  );

  const showImportPicker = vscode.commands.registerCommand(
    "solidity.showImportPicker",
    async (actions: { label: string; arguments: CodeAction }[]) => {
      const selected = await vscode.window.showQuickPick(actions, {
        placeHolder: "Choose import source:",
        matchOnDescription: true,
      });

      if (!selected) {
        return;
      }

      const params = selected.arguments as CodeAction;
      const resolvedAction = (await client.sendRequest(
        CodeActionResolveRequest.type,
        params,
      )) as CodeAction;
      if (!resolvedAction.edit) {
        return;
      }

      const workspaceEdit = await client.protocol2CodeConverter.asWorkspaceEdit(
        resolvedAction.edit,
      );
      await vscode.workspace.applyEdit(workspaceEdit);
    },
  );
  context.subscriptions.push(showImportPicker);

  const restartServerCommand = vscode.commands.registerCommand(
    "solstice.restartServer",
    async () => {
      log.info("Restarting Solstice server");

      // Stop existing client
      if (client) {
        await client.stop();
      }

      // Start fresh (this will re-run getServer() automatically)
      await startLanguageClient(context, config);
    },
  );
  context.subscriptions.push(restartServerCommand);

  startLanguageClient(context, config);
}

async function startLanguageClient(context: ExtensionContext, config: Config) {
  const command = await getServer();

  const run: Executable = {
    command,
    args: ["server"],
    transport: {
      // test
      kind: TransportKind.socket,
      port: 1111,
    },
    options: {
      env: {
        ...process.env,
        RUST_LOG: "debug",
      },
    },
  };

  const serverOptions: ServerOptions = {
    run,
    debug: run,
  };
  // If the extension is launched in debug mode then the debug server options are used
  // Otherwise the run options are used
  // Options to control the language client
  const clientOptions: LanguageClientOptions = {
    // Register the server for plain text documents
    documentSelector: [{ scheme: "file", language: "sol" }],
    middleware: createMiddleware(),
    initializationOptions: config.cfg,
    initializationFailedHandler: (error) => {
      return false;
    },
    synchronize: {
      // Notify the server about file changes to '.clientrc files contained in the workspace
      fileEvents: workspace.createFileSystemWatcher("**/.clientrc"),
    },
    traceOutputChannel,
  };

  // Create the language client and start the client.
  client = new LanguageClient(
    "Solstice",
    "solstice",
    serverOptions,
    clientOptions,
  );

  let outputChannel: vscode.OutputChannel;
  client.onNotification(
    "custom/logToChannel",
    (params: { channel: string; message: string }) => {
      if (!outputChannel) {
        outputChannel = vscode.window.createOutputChannel(params.channel, {
          log: true,
        });
      }
      outputChannel.replace(params.message);
      outputChannel.show(true); // Show but don't steal focus
    },
  );

  client.onNotification("custom/logToChannel2", (params) => {
    outputChannel.append("Starting debug session");
    outputChannel.append(JSON.stringify(params));

    // start vscode session
    vscode.debug.startDebugging(vscode.workspace.workspaceFolders[0], params);
  });

  context.subscriptions.push(client);
  client.start();
}

export function deactivate(): Thenable<void> | undefined {
  if (!client) {
    return undefined;
  }
  return client.stop();
}

class MockDebugAdapterNamedPipeServerDescriptorFactory
  implements vscode.DebugAdapterDescriptorFactory {
  private _server?: Net.Server;

  createDebugAdapterDescriptor(
    _session: vscode.DebugSession,
    _executable: vscode.DebugAdapterExecutable | undefined,
  ): vscode.ProviderResult<vscode.DebugAdapterDescriptor> {
    return new vscode.DebugAdapterServer(50051);
  }
}

class MockConfigurationProvider implements vscode.DebugConfigurationProvider {
  /**
   * Massage a debug configuration just before a debug session is being launched,
   * e.g. add all missing attributes to the debug configuration.
   */
  resolveDebugConfiguration(
    _folder: WorkspaceFolder | undefined,
    config: DebugConfiguration,
    _token?: CancellationToken,
  ): ProviderResult<DebugConfiguration> {
    // if launch.json is missing or empty
    if (!config.type && !config.request && !config.name) {
      const editor = vscode.window.activeTextEditor;
      if (editor && editor.document.languageId === "sol") {
        config.type = "mock";
        config.name = "Launch";
        config.request = "launch";
        config.program = "${file}";
        config.stopOnEntry = true;
      }
    }

    if (!config.program) {
      return vscode.window
        .showInformationMessage("Cannot find a program to debug")
        .then((_) => {
          return undefined; // abort launch
        });
    }

    return config;
  }
}

async function getServer(): Promise<string> {
  const configuration = vscode.workspace.getConfiguration("solstice");
  log.info("Loading configuration:", JSON.stringify(configuration));

  // Use process.env.SERVER_PATH in the launch configuration if defined
  if (process.env.SERVER_PATH) {
    log.info("Using server path from environment variable SERVER_PATH");
    return process.env.SERVER_PATH;
  }

  // Try to find the 'solstice' executable workspace configuration
  const serverPath = configuration.get<string>("serverPath");
  if (serverPath) {
    log.info("Using server path from workspace configuration");
    return serverPath;
  }

  // Try to find the 'solstice' executable in the $PATH
  if (executableFileExists("solstice")) {
    log.info("Using server path from $PATH");
    return "solstice";
  }

  // Try to find the latest release version of the 'solstice' executable
  const latestVersion = await getLatestReleaseVersion();
  log.info("Remote solstice version", latestVersion);

  // Try to find the 'solstice' executable in the $HOME/.solstice directory
  const home = configuration.get<string>("solsticePath") || os.homedir();

  const solsticePath = path.join(home, ".solstice", "solstice");
  if (executableFileExists(solsticePath)) {
    return await checkAndUpdateIfNeeded(solsticePath, latestVersion);
  }

  // No installation found - offer to install
  return await promptAndInstall(solsticePath, latestVersion);
}

async function checkAndUpdateIfNeeded(
  solsticePath: string,
  latestVersion: string,
): Promise<string> {
  try {
    const currentVersion = getSolsticeVersion(solsticePath);
    log.info("Current installed solstice version:", currentVersion);

    if (isNewerVersion(latestVersion, currentVersion)) {
      const action = await vscode.window.showInformationMessage(
        `Solstice update available: ${currentVersion} → ${latestVersion}`,
        "Update",
        "Skip",
      );

      if (action === "Update") {
        return await downloadSolsticeRelease(solsticePath, latestVersion);
      }
    }
  } catch (error) {
    log.error("Version check failed, using existing binary:", error);
  }

  return solsticePath;
}

async function promptAndInstall(
  solsticePath: string,
  latestVersion: string,
): Promise<string> {
  const action = await vscode.window.showInformationMessage(
    `Install Solstice ${latestVersion}?`,
    "Install",
    "Cancel",
  );

  if (action === "Install") {
    return await downloadSolsticeRelease(solsticePath, latestVersion);
  }

  const error = "Solstice installation cancelled";
  vscode.window.showErrorMessage(error);
  throw new Error(error);
}

function isNewerVersion(latest: string, current: string): boolean {
  // Remove 'v' prefix if present and compare
  const cleanLatest = latest.replace(/^v/, "");
  const cleanCurrent = current.replace(/^v/, "");

  const latestParts = cleanLatest.split(".").map(Number);
  const currentParts = cleanCurrent.split(".").map(Number);

  for (let i = 0; i < 3; i++) {
    // Assume semantic versioning (major.minor.patch)
    const l = latestParts[i] || 0;
    const c = currentParts[i] || 0;
    if (l > c) return true;
    if (l < c) return false;
  }

  return false; // Versions are equal
}

function executableFileExists(filePath: string): boolean {
  let exists = true;
  try {
    exists = fs.statSync(filePath).isFile();
    if (exists) {
      fs.accessSync(filePath, fs.constants.F_OK | fs.constants.X_OK);
    }
  } catch (e) {
    log.error(`File does not exist or is not executable: ${filePath}`, e);
    exists = false;
  }
  return exists;
}

// getSolsticeVersion executes the solstice executable with the --version flag and returns the version
function getSolsticeVersion(filePath: string): string {
  let version = execSync(`${filePath} --version`).toString();
  version = version.replace(/\s/g, "").trim(); // Remove whitespace and newlines

  // parse the version from the output
  const versionMatch = version.toString().match(/^solstice(\d+\.\d+\.\d+)$/);
  if (versionMatch) {
    return versionMatch[1];
  }

  throw new Error("Failed to get solstice version");
}

async function getLatestReleaseVersion(): Promise<string> {
  try {
    const response = await axios.get(
      "https://api.github.com/repos/ferranbt/solstice/releases/latest",
      {
        headers: {
          "User-Agent": "Node.js",
        },
      },
    );
    return response.data.tag_name;
  } catch (error) {
    log.error("Failed to fetch latest release:", error);
    return "";
  }
}

async function downloadSolsticeRelease(solsticePath: string, version: string) {
  const solsticeDir = path.dirname(solsticePath);

  // Create .solstice directory if it doesn't exist
  if (!fs.existsSync(solsticeDir)) {
    fs.mkdirSync(solsticeDir, { recursive: true });
  }

  // Determine the appropriate binary name based on platform
  const platform = process.platform;
  const arch = process.arch;
  let binaryName = "";

  if (platform === "darwin" && arch === "arm64") {
    binaryName = `solstice-${version}-aarch64-apple-darwin.tar.gz`;
  } else if (platform === "linux" && arch === "x64") {
    binaryName = `solstice-${version}-x86_64-unknown-linux-gnu.tar.gz`;
  } else {
    throw new Error(`Unsupported platform: ${platform} ${arch}`);
  }

  const downloadUrl = `https://github.com/ferranbt/solstice/releases/download/${version}/${binaryName}`;
  const tempFile = `/tmp/solstice_${version}.tar.gz`;

  log.info("Downloading solstice from", downloadUrl);

  // Download the file
  await new Promise((resolve, reject) => {
    const file = fs.createWriteStream(tempFile);
    axios
      .get(downloadUrl, { responseType: "stream" })
      .then((response) => {
        response.data.pipe(file);
        file.on("finish", () => {
          file.close();
          resolve(true);
        });
      })
      .catch(reject);
  });

  // Extract the tar.gz file
  await new Promise((resolve, reject) => {
    extract({
      file: tempFile,
      cwd: solsticeDir,
    })
      .then(() => {
        // Delete the tar.gz file after extraction
        fs.unlink(tempFile, (err) => {
          if (err) console.error("Error deleting temp file:", err);
        });
        resolve(true);
      })
      .catch(reject);
  });

  // Make the binary executable
  fs.chmodSync(solsticePath, 0o755);

  return solsticePath;
}
