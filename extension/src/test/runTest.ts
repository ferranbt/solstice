import * as path from "path";
import * as fs from "fs";

import { runTests } from "@vscode/test-electron";

async function main() {
  try {
    // The folder containing the Extension Manifest package.json
    // Passed to `--extensionDevelopmentPath`
    const extensionDevelopmentPath = path.resolve(__dirname, "../../");

    // The path to the extension test script
    // Passed to --extensionTestsPath
    const extensionTestsPath = path.resolve(__dirname, "./suite/index");

    // Remove the user data folder to reset all settings
    const userDataPath = path.resolve(
      __dirname,
      "../../.vscode-test/user-data",
    );
    if (fs.existsSync(userDataPath)) {
      fs.rmSync(userDataPath, { recursive: true, force: true });
      console.log("Cleared VS Code test user data");
    }

    // Download VS Code, unzip it and run the integration test
    await runTests({
      extensionDevelopmentPath,
      extensionTestsPath,
      launchArgs: ["--disable-extensions"],
      extensionTestsEnv: {
        ...process.env,
        SERVER_PATH:
          process.env.SERVER_PATH ||
          path.resolve(__dirname, "../../../target/debug/solstice"),
      },
    });
  } catch {
    console.error("Failed to run tests");
    process.exit(1);
  }
}

main();
