import * as assert from 'assert';
import * as vscode from 'vscode';
import * as path from 'path';
import { text } from 'stream/consumers';

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

    test('Format', async () => {
        // Open first the formatted doc to retrieve the expected output
        const formattedDoc = await open_document(getDocUri("formatted.sol"));
        const formattedTex = formattedDoc.getText();

        // Now open the unformatted document
        const unformattedDocURI = getDocUri("unformatted.sol");
        const unformattedDoc = await open_document(unformattedDocURI);

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

        console.log("Text Edits:", textedits);

        // Apply the text edits to the document and validate the result
        const workedits = new vscode.WorkspaceEdit();
        workedits.set(unformattedDocURI, textedits);

        const done = await vscode.workspace.applyEdit(workedits);
        assert.ok(done, 'Failed to apply text edits');

        console.log("Was it done?", done);

        const actualText = unformattedDoc.getText();

        // reset the changes before checking the results since we are going
        // to assert and we want to leave the document as it was
        for (let i = 0; i < textedits.length; i++) {
            await vscode.commands.executeCommand('undo');
        }

        assert.strictEqual(actualText, formattedTex, 'Formatted text does not match expected output');
    });
});
