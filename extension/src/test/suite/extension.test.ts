import * as assert from "assert";
import * as vscode from "vscode";
import * as path from "path";

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export const getDocPath = (p: string) => {
  return path.resolve(__dirname, "../../../src/testFixtures", p);
};

export function getDocUri(filePath: string): vscode.Uri {
  return vscode.Uri.file(getDocPath(filePath));
}

export async function open_document(
  uri: vscode.Uri,
): Promise<vscode.TextDocument | undefined> {
  try {
    const doc = await vscode.workspace.openTextDocument(uri);
    await vscode.window.showTextDocument(doc);
    await sleep(2000);
    return doc;
  } catch (error) {
    console.error("Error opening document:", error);
  }
}

async function activate() {
  const ext = vscode.extensions.getExtension(
    "ferranborreguero.solstice-language-server",
  );
  if (ext) {
    try {
      await ext.activate();
      console.log("Extension activated successfully");
    } catch (error) {
      console.error("Error activating extension:", error);
    }
  } else {
    console.error("Extension not found");
  }
}

suite("Extension Test Suite", () => {
  vscode.window.showInformationMessage("Start all tests.");

  suiteSetup(async () => {
    await activate();
  });

  test("Diagnostics", async () => {
    const uri = getDocUri("diag.sol");
    await open_document(uri);

    const diagnostics = vscode.languages.getDiagnostics(uri);
    assert.deepEqual(diagnostics.length, 1);

    const diagnostic = diagnostics[0];
    assert.strictEqual(diagnostic.severity, vscode.DiagnosticSeverity.Error);
    assert.strictEqual(diagnostic.message, "'value' not found");
    assert.strictEqual(diagnostic.source, "solstice");
    assert.strictEqual(diagnostic.code, "declaration_error");
    assert.deepStrictEqual(diagnostic.range, new vscode.Range(5, 8, 5, 13));
  });

  test("Hints", async () => {
    const uri = getDocUri("hints.sol");
    await open_document(uri);

    // Request inlay hints
    const hints = await vscode.commands.executeCommand<vscode.InlayHint[]>(
      "vscode.executeInlayHintProvider",
      uri,
      new vscode.Range(0, 0, 100, 0),
    );

    const expected_hints = [
      {
        label: "selector: 3d41c222",
        position: new vscode.Position(4, 74),
      },
      {
        label: "fn calculateFactorial_uint256",
        position: new vscode.Position(27, 5),
      },
      {
        label: "selector: fd610ec7",
        position: new vscode.Position(30, 60),
      },
    ];

    assert.strictEqual(
      hints.length,
      expected_hints.length,
      "Unexpected number of hints found",
    );
    for (let i = 0; i < hints.length; i++) {
      assert.strictEqual(
        hints[i].label,
        expected_hints[i].label,
        `Hint at index ${i} does not match expected label`,
      );
      assert.deepStrictEqual(
        hints[i].position,
        expected_hints[i].position,
        `Hint at index ${i} does not match expected position`,
      );
    }
  });

  test("Hover", async () => {
    const uri = getDocUri("simple.sol");
    const pos1 = new vscode.Position(7, 11);

    await open_document(uri);

    const hover = (await vscode.commands.executeCommand(
      "vscode.executeHoverProvider",
      uri,
      pos1,
    )) as vscode.Hover[];

    const contentarr1 = hover[0].contents as vscode.MarkdownString[];
    const content1 = contentarr1[0].value;
    assert.strictEqual(
      content1,
      "```solidity\nuint256 storage Parent.value3\n```",
    );
  });

  test("Definitions", async () => {
    const uri = getDocUri("defs.sol");
    await open_document(uri);

    const pos1 = new vscode.Position(11, 9);
    const action = (await vscode.commands.executeCommand(
      "vscode.executeDefinitionProvider",
      uri,
      pos1,
    )) as vscode.Location[];

    assert.deepStrictEqual(action[0].range, new vscode.Range(4, 19, 4, 25));
  });

  test("References", async () => {
    const uri = getDocUri("defs.sol");
    await open_document(uri);

    const pos1 = new vscode.Position(4, 20);
    const action = (await vscode.commands.executeCommand(
      "vscode.executeReferenceProvider",
      uri,
      pos1,
    )) as vscode.Location[];

    const expected = [
      new vscode.Range(4, 19, 4, 25),
      new vscode.Range(11, 8, 11, 14),
      new vscode.Range(12, 8, 12, 14),
      new vscode.Range(17, 15, 17, 21),
    ];

    assert.strictEqual(
      action.length,
      expected.length,
      "Unexpected number of references found",
    );
    for (let i = 0; i < action.length; i++) {
      assert.deepStrictEqual(
        action[i].range,
        expected[i],
        `Reference at index ${i} does not match expected range`,
      );
    }
  });

  test("Rename", async () => {
    const uri = getDocUri("rename.sol");
    await open_document(uri);

    const cases = [
      {
        new_name: "value_1",
        position: new vscode.Position(4, 21),
        expected: [
          new vscode.Range(4, 19, 4, 25),
          new vscode.Range(7, 8, 7, 14),
          new vscode.Range(7, 8, 7, 14), // Not sure why we have this duplicated, but the edits show the last item twice
        ],
      },
      {
        new_name: "value_2",
        position: new vscode.Position(6, 36),
        expected: [
          new vscode.Range(6, 35, 6, 41),
          new vscode.Range(7, 18, 7, 24),
        ],
      },
    ];

    for (const testCase of cases) {
      const newName = testCase.new_name;
      const pos1 = testCase.position;

      const edit = (await vscode.commands.executeCommand(
        "vscode.executeDocumentRenameProvider",
        uri,
        pos1,
        newName,
      )) as vscode.WorkspaceEdit;

      assert.ok(edit, "No workspace edit returned");
      assert.ok(edit.size > 0, "Workspace edit is empty");

      const locs = edit.get(uri) as vscode.TextEdit[];
      assert.strictEqual(
        locs.length,
        testCase.expected.length,
        `Expected ${testCase.expected.length} edits`,
      );

      for (let i = 0; i < locs.length; i++) {
        assert.deepStrictEqual(
          locs[i].range,
          testCase.expected[i],
          `Edit at index ${i} does not match expected range`,
        );
      }
    }
  });

  test("Format", async () => {
    const unformattedDocURI = getDocUri("unformatted.sol");
    await open_document(unformattedDocURI);

    const options = {
      tabSize: 4,
      insertSpaces: false,
    };
    const textedits = (await vscode.commands.executeCommand(
      "vscode.executeFormatDocumentProvider",
      unformattedDocURI,
      options,
    )) as vscode.TextEdit[];

    assert.ok(textedits.length > 0, "No text edits returned");

    for (let i = 0; i < textedits.length; i++) {
      await vscode.commands.executeCommand("undo");
    }
  });

  test("Completion", async () => {
    const uri = getDocUri("completion.sol");
    await open_document(uri);

    const get_labels = (list: vscode.CompletionList) =>
      list.items.map((item) => item.label);

    const pos1 = new vscode.Position(14, 10);
    const completions = (await vscode.commands.executeCommand(
      "vscode.executeCompletionItemProvider",
      uri,
      pos1,
      ".",
    )) as vscode.CompletionList;

    const labels = get_labels(completions);
    assert.ok(labels.includes("setValue"));
    assert.ok(labels.includes("val"));
  });

  test("Auto import", async () => {
    const uri = getDocUri("autoimport.sol");
    await open_document(uri);

    const pos1 = new vscode.Range(5, 8, 5, 14);
    const action1 = (await vscode.commands.executeCommand(
      "vscode.executeCodeActionProvider",
      uri,
      pos1,
    )) as (vscode.Command | vscode.CodeAction)[];

    // imports are grouped, so we expect one action
    assert.strictEqual(action1.length, 1);
    assert.strictEqual(action1[0].title, "Import... (2 options)");

    const pos2 = new vscode.Range(9, 8, 9, 19);
    const action2 = (await vscode.commands.executeCommand(
      "vscode.executeCodeActionProvider",
      uri,
      pos2,
    )) as (vscode.Command | vscode.CodeAction)[];

    // imports are grouped, so we expect one action
    assert.strictEqual(action2.length, 1);
    assert.strictEqual(action2[0].title, "Import Definitions from defs.sol");
  });

  test("Codelens test", async () => {
    // Code lens exist for functions that start with test_
    const uri = getDocUri("test.sol");
    await open_document(uri);

    const codeLenses = (await vscode.commands.executeCommand(
      "vscode.executeCodeLensProvider",
      uri,
    )) as vscode.CodeLens[];

    assert.strictEqual(codeLenses.length, 2);

    const position = new vscode.Range(4, 13, 4, 27);

    const test_lens = codeLenses[0];
    assert.strictEqual(test_lens.command.title, "run test");
    assert.strictEqual(test_lens.command.command, "sol.test.file");
    assert.deepStrictEqual(test_lens.range, position);

    // Execute the test command
    // If the command does not exists or it fails, the test call will fail
    // As of now, the commands do not return any value we can validate.
    await vscode.commands.executeCommand(
      test_lens.command.command,
      ...test_lens.command.arguments,
    );
  });

  test("Codelens debug", async () => {
    // Code lens exist for functions that start with test_
    const uri = getDocUri("test.sol");
    await open_document(uri);

    const codeLenses = (await vscode.commands.executeCommand(
      "vscode.executeCodeLensProvider",
      uri,
    )) as vscode.CodeLens[];

    assert.strictEqual(codeLenses.length, 2);

    const position = new vscode.Range(4, 13, 4, 27);

    const debug_lens = codeLenses[1];
    assert.strictEqual(debug_lens.command.title, "debug test");
    assert.strictEqual(debug_lens.command.command, "sol.debug.file");
    assert.deepStrictEqual(debug_lens.range, position);

    await vscode.commands.executeCommand(
      debug_lens.command.command,
      ...debug_lens.command.arguments,
    );
  });
});
