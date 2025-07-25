import * as assert from 'assert';
import * as vscode from 'vscode';
import * as path from 'path';

function sleep(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
}

export const getDocPath = (p: string) => {
    return path.resolve(__dirname, '../../../src/testFixtures', p);
};

function getDocUri(filePath: string): vscode.Uri {
    return vscode.Uri.file(getDocPath(filePath));
}

async function open_document(uri: vscode.Uri): Promise<vscode.TextDocument | undefined> {
    try {
        const doc = await vscode.workspace.openTextDocument(uri);
        await vscode.window.showTextDocument(doc);
        await sleep(2000);
        return doc;
    } catch (error) {
        console.error('Error opening document:', error);
    }
}

async function activate() {
    const ext = vscode.extensions.getExtension('ferranborreguero.solstice-language-server');
    if (ext) {
        try {
            await ext.activate();
            console.log('Extension activated successfully');
        } catch (error) {
            console.error('Error activating extension:', error);
        }
    } else {
        console.error('Extension not found');
    }
}

suite('Extension Test Suite', () => {
    vscode.window.showInformationMessage('Start all tests.');

    suiteSetup(async () => {
        await activate();
    });

    test('Hover', async () => {
        const uri = getDocUri("simple.sol");
        const pos1 = new vscode.Position(7, 11);

        await open_document(uri);

        const hover = (await vscode.commands.executeCommand(
            'vscode.executeHoverProvider',
            uri,
            pos1
        )) as vscode.Hover[];

        const contentarr1 = hover[0].contents as vscode.MarkdownString[];
        const content1 = contentarr1[0].value;
        assert.strictEqual(content1, '```solidity\nuint256 storage Parent.value3\n```');
    });

    test('Rename', async () => {
        const uri = getDocUri("rename.sol");
        await open_document(uri);

        const cases = [
            {
                'new_name': 'value_1',
                'position': new vscode.Position(4, 21),
                'expected': [
                    new vscode.Range(4, 19, 4, 25),
                    new vscode.Range(7, 8, 7, 14),
                    new vscode.Range(7, 8, 7, 14), // Not sure why we have this duplicated, but the edits show the last item twice
                ]
            },
            {
                'new_name': 'value_2',
                'position': new vscode.Position(6, 36),
                'expected': [
                    new vscode.Range(6, 35, 6, 41),
                    new vscode.Range(7, 18, 7, 24),
                ]
            }
        ]

        for (const testCase of cases) {
            const newName = testCase.new_name;
            const pos1 = testCase.position;

            console.log(`Renaming to: ${newName} at position: ${pos1}`);

            const edit = (await vscode.commands.executeCommand(
                'vscode.executeDocumentRenameProvider',
                uri,
                pos1,
                newName
            )) as vscode.WorkspaceEdit;

            assert.ok(edit, 'No workspace edit returned');
            assert.ok(edit.size > 0, 'Workspace edit is empty');

            const locs = edit.get(uri) as vscode.TextEdit[];
            assert.strictEqual(locs.length, testCase.expected.length, `Expected ${testCase.expected.length} edits`);

            for (let i = 0; i < locs.length; i++) {
                assert.deepStrictEqual(locs[i].range, testCase.expected[i], `Edit at index ${i} does not match expected range`);
            }
        }
    });

    test('Format', async () => {
        const unformattedDocURI = getDocUri("unformatted.sol");
        await open_document(unformattedDocURI);

        const options = {
            tabSize: 4,
            insertSpaces: false,
        };
        const textedits = (await vscode.commands.executeCommand(
            'vscode.executeFormatDocumentProvider',
            unformattedDocURI,
            options,
        )) as vscode.TextEdit[];

        assert.ok(textedits.length > 0, 'No text edits returned');

        for (let i = 0; i < textedits.length; i++) {
            await vscode.commands.executeCommand('undo');
        }
    });
});
