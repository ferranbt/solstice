import * as assert from 'assert';
import * as vscode from 'vscode';

function sleep(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function open_document(doc_path: string) {
    const uri = vscode.Uri.file(doc_path);

    try {
        const doc = await vscode.workspace.openTextDocument(uri);
        await vscode.window.showTextDocument(doc);
        await sleep(2000);
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

    test('Sample test', async () => {
        await activate();
        await open_document("/Users/ferranbt/go/src/github.com/ferranbt/solstice/extension/src/testFixtures/simple.sol");
        await sleep(200000);
    });
});
