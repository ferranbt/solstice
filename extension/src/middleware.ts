import * as vscode from 'vscode';
import * as lc from 'vscode-languageclient/node';
import { consoleLog } from './extension';

export function createMiddleware(): lc.Middleware {
    return {
        async provideCodeActions(
            document: vscode.TextDocument,
            range: vscode.Range,
            context: vscode.CodeActionContext,
            token: vscode.CancellationToken,
            next: lc.ProvideCodeActionsSignature,
        ): Promise<(vscode.Command | vscode.CodeAction)[] | undefined> {
            consoleLog('🎯 MIDDLEWARE CALLED! provideCodeActions');
            consoleLog(`📄 Document: ${document.uri.toString()}`);
            consoleLog(`📍 Range: ${range.start.line}:${range.start.character} to ${range.end.line}:${range.end.character}`);

            const actions = await next(document, range, context, token);
            if (!actions || actions.length === 0) {
                return actions;
            }

            consoleLog(`📦 Found actions`, JSON.stringify(actions));

            // If multiple actions, group them
            if (actions.length > 1) {
                consoleLog(`🎯 Multiple actions (${actions.length}), creating grouped action`);

                const groupedAction = new vscode.CodeAction(
                    `Import... (${actions.length} options)`,
                    vscode.CodeActionKind.QuickFix
                );

                // Convert all actions to have proper LSP format for resolve
                const importActions = actions.map(action => {
                    if (action instanceof vscode.CodeAction) {
                        // Create a copy with fixed kind for LSP resolve
                        const fixedAction = {
                            title: action.title,
                            kind: action.kind?.value || 'quickfix', // Fix: extract string from VS Code format
                            data: (action as any).data, // Access the raw data
                            isPreferred: action.isPreferred,
                            // Don't include edit - that's for resolve
                        };

                        consoleLog('🔧 Fixed action for LSP:', JSON.stringify(fixedAction, null, 2));

                        return { label: fixedAction.title, arguments: fixedAction };
                    }
                    return { label: action.title, arguments: action }; // Fallback for non-CodeAction types
                }) as any[]; // Cast to avoid type issues

                groupedAction.command = {
                    command: 'solidity.showImportPicker',
                    title: 'Show Import Options',
                    arguments: [importActions]
                };

                groupedAction.edit = new vscode.WorkspaceEdit();
                groupedAction.isPreferred = true;

                consoleLog('✅ Created grouped action');
                return [groupedAction];
            }

            // Single action, return as-is
            consoleLog('📤 Single action, returning unchanged');
            return actions;
        }
    }
}
