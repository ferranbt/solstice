import * as vscode from 'vscode';
import * as lc from 'vscode-languageclient/node';

export function createMiddleware(): lc.Middleware {
    return {
        async provideCodeActions(
            document: vscode.TextDocument,
            range: vscode.Range,
            context: vscode.CodeActionContext,
            token: vscode.CancellationToken,
            next: lc.ProvideCodeActionsSignature,
        ): Promise<(vscode.Command | vscode.CodeAction)[] | undefined> {
            const actions = await next(document, range, context, token);
            if (!actions || actions.length === 0) {
                return actions;
            }

            // If multiple actions, group them
            if (actions.length > 1) {
                const groupedAction = new vscode.CodeAction(
                    `Import... (${actions.length} options)`,
                    vscode.CodeActionKind.QuickFix
                );

                // Convert all actions to have proper LSP format for resolve
                const importActions = actions.map(action => {
                    if (action instanceof vscode.CodeAction) {
                        const fixedAction = {
                            title: action.title,
                            kind: action.kind?.value,
                            data: (action as any).data,
                            isPreferred: action.isPreferred,
                        };
                        return { label: fixedAction.title, arguments: fixedAction };
                    }
                    return { label: action.title, arguments: action }; // Fallback for non-CodeAction types
                }) as any[];

                groupedAction.command = {
                    command: 'solidity.showImportPicker',
                    title: 'Show Import Options',
                    arguments: [importActions]
                };

                groupedAction.edit = new vscode.WorkspaceEdit();
                groupedAction.isPreferred = true;

                return [groupedAction];
            }

            // Single action, return as-is
            return actions;
        }
    }
}
