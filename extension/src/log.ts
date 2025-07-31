import * as vscode from "vscode";

class Log {
  private readonly output = vscode.window.createOutputChannel(
    "solstice Extension",
    {
      log: true,
    },
  );

  trace(...messages: [unknown, ...unknown[]]): void {
    this.output.trace(JSON.stringify(messages));
  }

  debug(...messages: [unknown, ...unknown[]]): void {
    this.output.debug(JSON.stringify(messages));
  }

  info(...messages: [unknown, ...unknown[]]): void {
    this.output.info(JSON.stringify(messages));
  }

  warn(...messages: [unknown, ...unknown[]]): void {
    this.output.warn(JSON.stringify(messages));
  }

  error(...messages: [unknown, ...unknown[]]): void {
    this.output.error(JSON.stringify(messages));
    this.output.show(true);
  }
}

export const log = new Log();
