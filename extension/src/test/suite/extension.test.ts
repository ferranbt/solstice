import * as assert from 'assert';
import * as vscode from 'vscode';

function sleep(ms: number): Promise<void> {
    return new Promise(resolve => setTimeout(resolve, ms));
}

async function open_document(uri: vscode.Uri) {
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
        console.log('Running sample test...');

        await activate();
        console.log('Running sample test...2');
        const uri = vscode.Uri.file("/Users/ferranbt/go/src/github.com/ferranbt/solstice/extension/src/testFixtures/simple.sol");
        const pos1 = new vscode.Position(8, 11);
        console.log('Running sample test...3');
        await open_document(uri);

        /*console.log('Running sample test...4');
        const hover = (await vscode.commands.executeCommand(
            'vscode.executeHoverProvider',
            uri,
            pos1
        )) as vscode.Hover[];
        console.log('Running sample test...5');
        console.log('6', hover);
        */

        await sleep(200000);
    });
});
