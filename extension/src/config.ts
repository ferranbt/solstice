import * as vscode from "vscode";
import { log } from "./log";

export class Config {
  readonly extensionId = "ferranborreguero.solstice-language-server";
  readonly rootSection = "solstice";

  private readonly requiresServerReloadOpts = ["inlayHints"].map(
    (opt) => `${this.rootSection}.${opt}`,
  );

  constructor() {
    vscode.workspace.onDidChangeConfiguration(
      this.onDidChangeConfiguration,
      this,
    );
    this.refreshLogging();
  }

  get cfg(): vscode.WorkspaceConfiguration {
    return vscode.workspace.getConfiguration(this.rootSection);
  }

  private refreshLogging() {
    log.info(
      "Extension version:",
      vscode.extensions.getExtension(this.extensionId)!.packageJSON.version,
    );

    const cfg = Object.entries(this.cfg).filter(
      ([_, val]) => !(val instanceof Function),
    );
    log.info("Using configuration", Object.fromEntries(cfg));
  }

  private async onDidChangeConfiguration(
    event: vscode.ConfigurationChangeEvent,
  ) {
    this.refreshLogging();

    const requiresServerReloadOpt = this.requiresServerReloadOpts.find((opt) =>
      event.affectsConfiguration(opt),
    );

    if (!requiresServerReloadOpt) return;

    if (this.restartServerOnConfigChange) {
      await vscode.commands.executeCommand("solstice.restartServer");
      return;
    }

    const message = `Changing the configuration requires a server restart`;
    const userResponse = await vscode.window.showInformationMessage(
      message,
      "Restart now",
    );

    if (userResponse) {
      const command = "solstice.restartServer";
      await vscode.commands.executeCommand(command);
    }
  }

  get restartServerOnConfigChange() {
    return this.get<boolean>("restartServerOnConfigChange");
  }

  private get<T>(path: string): T | undefined {
    return this.cfg.get<T>(path);
  }
}
