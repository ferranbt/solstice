import * as assert from "assert";
import * as vscode from "vscode";
import * as path from "path";
import * as sinon from "sinon";
import * as fs from "fs";
import { getDocUri, open_document, sleep } from "./extension.test";
import { updateConfig } from "./extension.test";

suite("Download LSP server", () => {
  let originalServerPath: string | undefined;

  setup(async () => {
    originalServerPath = process.env.SERVER_PATH;
  });

  teardown(async () => {
    process.env.SERVER_PATH = originalServerPath;
  });

  test("Download and check server", async () => {
    // Set up stub to auto-click "Install" when modal appears
    // We need to set the stub before the extension is reactivated
    const installStub = sinon.stub(vscode.window, "showInformationMessage");
    installStub.resolves("Install"); // Auto-click "Install"

    delete process.env.SERVER_PATH;

    // generate a temporal folder to store the solstice path
    const tmpDir = path.join(__dirname, "tmp");
    await updateConfig("solsticePath", tmpDir);

    // Restart the solstice extension to apply the new configuration
    await vscode.commands.executeCommand("solstice.restartServer");

    const uri = getDocUri("diag.sol");
    await open_document(uri);

    // verify the modal was shown
    assert.strictEqual(installStub.callCount, 1);
    const call = installStub.firstCall;
    const message = call.args[0];
    assert.ok(message.startsWith("Install Solstice"));

    // verify the solstice LSP binary was downloaded
    const solsticeBinPath = path.join(tmpDir, ".solstice", "solstice");
    assert.ok(
      fs.existsSync(solsticeBinPath),
      "Solstice binary was not downloaded",
    );

    // Wait for the extension to finish setup and check if the extension got activated
    await sleep(2000);
    const diagnostics = vscode.languages.getDiagnostics(uri);
    assert.deepEqual(diagnostics.length, 1);

    installStub.restore();

    await updateConfig("solsticePath", undefined);
    fs.rmdirSync(tmpDir, { recursive: true });
  });
});
